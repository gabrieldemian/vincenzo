//! Disk is responsible for file I/O of all torrents.

use crate::{
    bitfield::{Bitfield, VczBitfield},
    config::ResolvedConfig,
    daemon::{DaemonCtx, DaemonMsg},
    error::Error,
    extensions::{
        BLOCK_LEN, MetadataPiece,
        core::{Block, BlockInfo},
    },
    metainfo::{Info, MetaInfo},
    peer::{PeerCtx, PeerId},
    torrent::{
        InfoHash, Torrent, TorrentCtx, TorrentMsg, TorrentStatus,
        TorrentStatusErrorCode,
    },
    utils::to_human_readable,
};
use bendy::{decoding::FromBencode, encoding::ToBencode};
use bytes::Bytes;
use futures::future::join_all;
use lru::LruCache;
use memmap2::{Mmap, MmapMut};
use rand::seq::SliceRandom;
use rayon::iter::{
    IntoParallelIterator, IntoParallelRefIterator, ParallelIterator,
};
use sha1::{Digest, Sha1};
use std::{
    collections::HashMap,
    net::SocketAddr,
    num::NonZeroUsize,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    fs::{File, OpenOptions},
    io::{AsyncReadExt, AsyncWriteExt},
    select,
    sync::{
        Mutex,
        mpsc::{self, Receiver},
        oneshot::{self, Sender},
    },
    time::{Instant, interval},
};
use tracing::{debug, error, info, warn};

/// There are 3 dirs inside `torrents` to store metainfo files in their
/// contexts.
#[derive(Clone, Copy, PartialEq)]
#[repr(u8)]
pub enum MetadataDir {
    /// Metainfos that are seeding and fully downloaded
    Complete,
    /// Downloading metainfo, on completion moved to complete.
    Incomplete,
    /// Metainfo to be downloaded and moved to incomplete.
    Queue,
}

#[derive(Debug)]
pub enum DiskMsg {
    /// Sent by the frontend or CLI flag to add a new torrent from a magnet,
    /// just after the metainfo was downloaded.
    AddTorrent(Arc<TorrentCtx>, Arc<Info>),

    GetTorrentCtx(InfoHash, oneshot::Sender<Option<Arc<TorrentCtx>>>),

    MetadataSize(InfoHash, usize),

    /// The Peer does not have an ID until the handshake, when that happens,
    /// this message will be sent immediately to add the peer context.
    NewPeer(Arc<PeerCtx>),

    DeletePeer(SocketAddr),

    DeleteTorrent(InfoHash),

    Endgame(InfoHash),

    FinishedDownload(InfoHash),

    ReadBlock {
        info_hash: InfoHash,
        block_info: BlockInfo,
        recipient: Sender<Bytes>,
    },

    /// Handle a new downloaded Piece, validate that the hash all the blocks of
    /// this piece matches the hash on Info.pieces. If the hash is valid,
    /// the fn will send a Have msg to all peers that don't have this piece.
    /// and update the bitfield of the Torrent struct.
    ValidatePiece {
        info_hash: InfoHash,
        recipient: Sender<Result<(), Error>>,
        piece: usize,
    },

    /// Write the given block to disk, the Disk struct will get the seeked file
    /// automatically.
    WriteBlock {
        info_hash: InfoHash,
        block: Block,
    },

    /// Request block infos that the peer has, that we do not have ir nor
    /// requested it.
    RequestBlocks {
        peer_id: PeerId,
        recipient: Sender<Vec<BlockInfo>>,
        qnt: usize,
    },

    /// Request a piece of the metadata.
    RequestMetadata {
        info_hash: InfoHash,
        recipient: Sender<Vec<MetadataPiece>>,
        qnt: usize,
    },

    GetPeerCtx {
        peer_id: PeerId,
        recipient: Sender<Option<Arc<PeerCtx>>>,
    },

    Quit,
}

/// The algorithm that determines how pieces are downloaded.
/// The recommended is Random. But Sequential is used for streaming.
///
/// The default algorithm to use is random-first until we have
/// a complete piece, after that, we switch to rarest-first.
#[derive(Clone, Copy, Hash, PartialEq, Eq, Default, Debug)]
pub enum PieceStrategy {
    /// Random-first, select random pieces to download
    #[default]
    Random,
    /// Rarest-first, give priority to the rarest pieces.
    Rarest,
    /// Sequential downloads, useful in streaming.
    Sequential,
}

// Precomputed file metadata for fast lookups
#[derive(Debug, Clone)]
pub struct FileMetadata {
    pub path: Arc<Path>,
    pub start_offset: usize,
    pub length: usize,
}

// Cache entry with precomputed metadata
#[derive(Debug, Clone)]
pub struct TorrentCache {
    pub file_metadata: Vec<FileMetadata>,
    pub piece_length: usize,
    pub total_size: usize,
}

/// When a peer is Choked, or receives an error and must close the
/// connection, the outgoing/pending blocks of this peer must be
/// appended back to the list of available block_infos.
pub enum ReturnToDisk {
    Block(InfoHash, Vec<BlockInfo>),
    Metadata(InfoHash, Vec<MetadataPiece>),
}

/// The Disk struct responsabilities:
/// - Open and create files, create directories
/// - Read/Write blocks to files
/// - Store block infos of all torrents
/// - Validate hash of pieces
pub struct Disk {
    pub config: Arc<ResolvedConfig>,
    pub tx: mpsc::Sender<DiskMsg>,
    pub daemon_ctx: Arc<DaemonCtx>,
    pub rx: Receiver<DiskMsg>,
    pub free_rx: mpsc::UnboundedReceiver<ReturnToDisk>,
    pub peer_ctxs: Vec<Arc<PeerCtx>>,
    pub torrent_ctxs: HashMap<InfoHash, Arc<TorrentCtx>>,

    pub piece_strategy: HashMap<InfoHash, PieceStrategy>,

    /// How many pieces were downloaded.
    pub downloaded_pieces: HashMap<InfoHash, u64>,

    pub endgame: HashMap<InfoHash, bool>,

    /// A cache of blocks, where the key is a piece. The cache will be cleared
    /// when the entire piece is downloaded and validated as it will be
    /// written to disk.
    // usize = how many bytes written
    pub block_cache: HashMap<InfoHash, HashMap<usize, (usize, Vec<u8>)>>,

    pub torrent_info: HashMap<InfoHash, Arc<Info>>,

    /// The block infos of each piece of a torrent.
    pub block_infos: HashMap<InfoHash, Vec<BlockInfo>>,

    pub metadata_pieces: HashMap<InfoHash, Vec<MetadataPiece>>,

    /// A cache of torrent files with pre-computed lengths.
    pub torrent_cache: HashMap<InfoHash, TorrentCache>,

    /// Files that need to be flushed, the bitfield is relative to
    /// `torrent_cache` files.
    pub dirty_files: HashMap<InfoHash, Bitfield>,

    /// A LRU cache of file handles to avoid doing a sys call each time the
    /// disk needs to read or write to a file.
    pub read_mmap_cache: LruCache<Arc<Path>, Arc<Mmap>>,
    pub write_mmap_cache: LruCache<Arc<Path>, Arc<Mutex<MmapMut>>>,

    /// The instant that a file was last accessed.
    pub last_accessed: HashMap<Arc<Path>, Instant>,
}

/// Cache capacity of files.
static FILE_CACHE_CAPACITY: usize = 512;

/// For how long a file must be inactive to be deleted from the LRU cache.
static DURATION_LRU_POP: Duration = Duration::from_millis(10_000);

impl Disk {
    pub fn new(
        config: Arc<ResolvedConfig>,
        daemon_ctx: Arc<DaemonCtx>,
        tx: mpsc::Sender<DiskMsg>,
        rx: mpsc::Receiver<DiskMsg>,
        free_rx: mpsc::UnboundedReceiver<ReturnToDisk>,
    ) -> Self {
        Self {
            config,
            dirty_files: HashMap::new(),
            daemon_ctx,
            free_rx,
            rx,
            tx,
            read_mmap_cache: LruCache::new(
                NonZeroUsize::new(FILE_CACHE_CAPACITY).unwrap(),
            ),
            write_mmap_cache: LruCache::new(
                NonZeroUsize::new(FILE_CACHE_CAPACITY).unwrap(),
            ),
            torrent_cache: HashMap::new(),
            metadata_pieces: HashMap::new(),
            block_cache: HashMap::new(),
            peer_ctxs: Vec::new(),
            torrent_ctxs: HashMap::new(),
            downloaded_pieces: HashMap::new(),
            endgame: HashMap::new(),
            piece_strategy: HashMap::default(),
            block_infos: HashMap::default(),
            torrent_info: HashMap::default(),
            last_accessed: HashMap::default(),
        }
    }

    #[tracing::instrument(name = "disk", skip_all)]
    pub async fn run(&mut self) -> Result<(), Error> {
        let mut flush_interval = interval(Duration::from_millis(1_000));
        let mut dirty_count_history = Vec::with_capacity(10);
        let mut lru_cleanup_interval = interval(Duration::from_millis(11_000));

        // ensure the necessary folders are created.
        // if tokio::fs::metadata(self.incomplete_torrents_path()).await.
        // is_err() { };
        //
        // if tokio::fs::metadata(self.incomplete_torrents_path()).await.
        // is_err() { }
        //
        // if tokio::fs::metadata(self.queue_torrents_path()).await.is_err() {
        // }
        tokio::fs::create_dir_all(self.incomplete_torrents_path()).await?;
        tokio::fs::create_dir_all(self.complete_torrents_path()).await?;
        tokio::fs::create_dir_all(self.queue_torrents_path()).await?;

        // load .torrent files into the client.
        self.read_metainfos_and_add(MetadataDir::Complete).await?;
        self.read_metainfos_and_add(MetadataDir::Incomplete).await?;
        self.read_metainfos_and_add(MetadataDir::Queue).await?;

        'outer: loop {
            select! {
                Some(msg) = self.rx.recv() => {
                    match msg {
                        DiskMsg::GetTorrentCtx(info_hash, sender) => {
                            let v = self.torrent_ctxs.get(&info_hash).cloned();
                            let _ = sender.send(v);
                        }
                        DiskMsg::GetPeerCtx { peer_id, recipient } => {
                            let p = self.peer_ctxs.iter().find(|p| p.id == peer_id).cloned();
                            let _ = recipient.send(p);
                        }
                        DiskMsg::DeleteTorrent(info_hash) => {
                            let path = self.get_metainfo_path(&info_hash)?;
                            tokio::fs::remove_file(path).await?;
                            self.torrent_cache.remove_entry(&info_hash);
                            self.block_cache.remove_entry(&info_hash);
                            self.torrent_ctxs.remove_entry(&info_hash);
                            self.downloaded_pieces.remove_entry(&info_hash);
                            self.endgame.remove_entry(&info_hash);
                            self.piece_strategy.remove_entry(&info_hash);
                            self.block_infos.remove_entry(&info_hash);
                            self.metadata_pieces.remove_entry(&info_hash);
                            self.torrent_info.remove_entry(&info_hash);
                            self.dirty_files.remove_entry(&info_hash);
                            self.peer_ctxs.retain(|v| v.torrent_ctx.info_hash != info_hash);
                        }
                        DiskMsg::FinishedDownload(info_hash) => {
                            let _ = self.write_complete_torrent_metainfo(&info_hash).await;
                        }
                        DiskMsg::MetadataSize(info_hash, size) => {
                            if self.metadata_pieces.contains_key(
                                &info_hash,
                            ) { continue };

                            let pieces = size.div_ceil(BLOCK_LEN);
                            info!("meta pieces {pieces}");

                            let pieces =
                                (0..pieces)
                                .map(MetadataPiece).collect();

                            self.metadata_pieces.insert(
                                info_hash,
                                pieces,
                            );
                        }
                        DiskMsg::Endgame(info_hash) => {
                            let _ = self.enter_endgame(info_hash).await;
                        }
                        DiskMsg::DeletePeer(addr) => {
                            self.delete_peer(addr);
                        }
                        DiskMsg::AddTorrent(torrent, info) => {
                            let _ = self.add_torrent(torrent, info).await;
                        }
                        DiskMsg::ReadBlock { block_info, recipient, info_hash } => {
                            let block = self.read_block(&info_hash, &block_info).await?;
                            let _ = recipient.send(block);
                        }
                        DiskMsg::WriteBlock { block, info_hash } => {
                            let _ = self.write_block(&info_hash, block).await;
                        }
                        DiskMsg::RequestBlocks { qnt, recipient, peer_id } => {
                            let infos = self
                                .request_blocks(&peer_id, qnt)
                                .await
                                .unwrap_or_default();

                            let _ = recipient.send(infos);
                        }
                        DiskMsg::RequestMetadata { qnt, recipient, info_hash } => {
                            let pieces = self
                                .request_metadata(&info_hash, qnt)
                                .unwrap_or_default();

                            let _ = recipient.send(pieces);
                        }
                        DiskMsg::ValidatePiece { info_hash, recipient, piece } => {
                            let r = self.validate_piece(&info_hash, piece).await;
                            let _ = recipient.send(r);
                        }
                        DiskMsg::NewPeer(peer) => {
                            self.new_peer(peer);
                        }
                        DiskMsg::Quit => {
                            debug!("Quit");
                            break 'outer Ok(());
                        }
                    }
                }
                // if a file is inactive for `DURATION_LRU_POP` delete it
                // from the mmap caches.
                _ = lru_cleanup_interval.tick() => {
                    let mut to_retain = Bitfield::from_piece_true(self.last_accessed.len());
                    for (i, (k, v)) in self.last_accessed.iter().enumerate() {
                        if Instant::now() - *v > DURATION_LRU_POP {
                            self.read_mmap_cache.pop(k);
                            self.write_mmap_cache.pop(k);
                            to_retain.set(i, false);
                        }
                    }
                    let mut iter = to_retain.into_iter();
                    self.last_accessed.retain(|_, _| iter.next().unwrap());
                }
                _ = flush_interval.tick() => {
                    let total_dirty: usize = self.dirty_files.values()
                        .map(|bits| bits.count_ones())
                        .sum();

                    dirty_count_history.push(total_dirty);

                    if dirty_count_history.len() > 10 {
                        dirty_count_history.swap_remove(0);
                    }

                    let avg_dirty = dirty_count_history.iter().sum::<usize>()
                        / dirty_count_history.len().max(1);

                    flush_interval = if avg_dirty > 50 {
                         // aggressive flushing for high activity
                        interval(Duration::from_millis(10))
                    } else if avg_dirty > 10 {
                        interval(Duration::from_millis(100))
                    } else if avg_dirty > 0 {
                        interval(Duration::from_millis(500))
                    } else {
                        interval(Duration::from_millis(1000));
                        continue
                    };

                    self.flush_dirty_files().await;
                    self.dirty_files.retain(|_, bits| bits.any());
                }
                Some(return_to_disk) = self.free_rx.recv() => {
                    match return_to_disk {
                        ReturnToDisk::Block(info_hash, blocks) => {
                            if self.endgame.get(&info_hash).copied()
                                .unwrap_or(false)
                            {
                                self
                                    .torrent_ctxs
                                    .get_mut(&info_hash)
                                    .ok_or(Error::TorrentDoesNotExist)?
                                    .tx
                                    .send(TorrentMsg::Endgame(blocks)).await?;
                            } else {
                                self.block_infos
                                    .entry(info_hash)
                                    .or_default()
                                    .extend(blocks);
                            }
                        }
                        ReturnToDisk::Metadata(info_hash, pieces) => {
                            self
                                .metadata_pieces
                                .get_mut(&info_hash)
                                .ok_or(Error::TorrentDoesNotExist)?
                                .extend(pieces);
                        }
                    };
                }
            }
        }
    }

    /// Adds a new torrent to Disk. New torrents will be added by a magnet URL
    /// in the high parts of the code, to the bottom (Disk).
    async fn add_torrent(
        &mut self,
        torrent_ctx: Arc<TorrentCtx>,
        info: Arc<Info>,
    ) -> Result<(), Error> {
        debug!("new torrent {:?}", info.info_hash);
        let info_hash = info.info_hash.clone();

        self.torrent_ctxs.insert(info_hash.clone(), torrent_ctx.clone());
        self.torrent_info.insert(info_hash.clone(), info.clone());

        // if the client already has a .torrent file, it's a duplicate.
        let is_complete = self.is_torrent_complete(&info.name);
        let is_incomplete = self.is_torrent_incomplete(&info.name);

        if is_complete || is_incomplete {
            return Err(Error::NoDuplicateTorrent);
        }

        self.compute_torrent_cache(&info);

        // create folders, files, and preallocate them.
        self.pre_alloc_files(&info_hash).await?;

        let downloaded_pieces = self.compute_downloaded_pieces(&info).await?;

        // send msg to torrent with some information,
        // the bytes downloaded from pieces and also the bitfield from it.
        let b = downloaded_pieces.count_ones() * info.piece_length;
        debug!("already downloaded {b} bytes");

        let info: Info = (*info).clone();
        self.write_incomplete_torrent_metainfo(info).await?;

        self.compute_torrent_state(&info_hash, &downloaded_pieces).await?;

        Ok(())
    }

    /// Create a new torrent from a [`MetaInfo`] and send a
    /// [`DaemonMsg::AddTorrentMetaInfo`].
    #[tracing::instrument(skip_all, fields(name = %metainfo.info.name))]
    pub async fn new_torrent_metainfo(
        &mut self,
        metainfo: MetaInfo,
    ) -> Result<(), Error> {
        debug!("new torrent {:?}", metainfo.info.info_hash);

        let info_hash = metainfo.info.info_hash.clone();
        let info = Arc::new(metainfo.info.clone());

        self.torrent_info.insert(info_hash.clone(), info);

        self.compute_torrent_cache(&metainfo.info);

        // create folders, files, and preallocate them with zeroes.
        self.pre_alloc_files(&info_hash).await?;

        let mut is_err = false;
        let downloaded_pieces =
            match self.compute_downloaded_pieces(&metainfo.info).await {
                Ok(downloaded_pieces) => downloaded_pieces,
                Err(Error::TorrentFilesMissing(downloaded_pieces)) => {
                    println!("hehhhhhhhh");
                    is_err = true;
                    downloaded_pieces
                }
                Err(e) => return Err(e),
            };

        self.compute_torrent_state(&info_hash, &downloaded_pieces).await?;

        let b = downloaded_pieces.count_ones() * metainfo.info.piece_length;
        info!("downloaded {:?}", to_human_readable(b as f64));

        let mut torrent = Torrent::new_metainfo(
            self.config.clone(),
            self.tx.clone(),
            self.daemon_ctx.clone(),
            metainfo,
            downloaded_pieces,
        );

        if is_err {
            error!("missing files on disk");
            torrent.status =
                TorrentStatus::Error(TorrentStatusErrorCode::FilesMissing);
        }

        self.torrent_ctxs.insert(info_hash.clone(), torrent.ctx.clone());

        self.daemon_ctx.tx.send(DaemonMsg::AddTorrentMetaInfo(torrent)).await?;

        Ok(())
    }

    async fn preopen_files<'a>(
        &self,
        file_metadata: &'a [FileMetadata],
    ) -> Result<
        Vec<(&'a FileMetadata, Arc<Mmap>)>,
        Vec<(&'a FileMetadata, Arc<Mmap>)>,
    > {
        let mut mmaps = Vec::with_capacity(file_metadata.len());

        for file_meta in file_metadata {
            let path = file_meta.path.as_ref().to_owned();
            println!("2222222 {path:?}");
            // return error if the file doesn't exist
            // if tokio::fs::metadata(&path).await.is_err() {
            //     println!("its just not possible {path:?}");
            //     return Err(mmaps);
            // };
            let file = Self::open_file(path).await.expect("cant preopen");
            let mmap = unsafe { Mmap::map(&file).unwrap() };
            mmaps.push((file_meta, Arc::new(mmap)));
        }

        Ok(mmaps)
    }

    /// Compute the downloaded and valid pieces of the torrent and feed the read
    /// cache with the file handles.
    #[tracing::instrument(skip_all, fields(name = %info.name))]
    async fn compute_downloaded_pieces(
        &mut self,
        info: &Info,
    ) -> Result<Bitfield, Error> {
        let torrent_cache = self
            .torrent_cache
            .get(&info.info_hash)
            .ok_or(Error::TorrentDoesNotExist)?;

        let expected_hashes: Vec<[u8; 20]> = info
            .pieces
            .chunks_exact(20)
            .map(|chunk| chunk.try_into().unwrap())
            .collect();

        let total_pieces = info.pieces();
        let piece_length = info.piece_length;
        let total_size = torrent_cache.total_size;
        let file_handles =
            self.preopen_files(&torrent_cache.file_metadata).await;

        let is_err = file_handles.is_err();

        let file_handles = match file_handles {
            Ok(v) => v,
            Err(v) => v,
        };

        // compute all pieces in parallel using rayon.
        let piece_results: Vec<bool> = (0..total_pieces)
            .into_par_iter()
            .map(|piece_index| {
                Self::verify_piece(
                    piece_index,
                    file_handles.as_slice(),
                    piece_length,
                    total_size,
                    expected_hashes[piece_index],
                )
            })
            .collect();

        // reuse the already open file handles and put in the cache.
        for (f, mmap) in file_handles {
            self.read_mmap_cache.put(f.path.clone(), mmap);
        }

        let mut downloaded_pieces = Bitfield::from_piece(total_pieces);

        for (piece_index, result) in piece_results.into_iter().enumerate() {
            downloaded_pieces.set(piece_index, result);
        }

        info!(
            "computed {} pieces, {} downloaded",
            downloaded_pieces.len(),
            downloaded_pieces.count_ones()
        );

        if is_err {
            println!("helpppp");
            return Err(Error::TorrentFilesMissing(downloaded_pieces));
        }
        Ok(downloaded_pieces)
    }

    /// Compute the state of the torrent, how many missing pieces, the block
    /// infos, etc. This should be called after computing the pieces with
    /// `compute_all_pieces`.
    async fn compute_torrent_state(
        &mut self,
        info_hash: &InfoHash,
        downloaded_pieces: &Bitfield,
    ) -> Result<(), Error> {
        let info = self
            .torrent_info
            .get(info_hash)
            .ok_or(Error::TorrentDoesNotExist)?;

        let downloaded_count = downloaded_pieces.count_ones() as u64;

        self.downloaded_pieces.insert(info_hash.clone(), downloaded_count);

        let block_infos =
            self.block_infos.entry(info_hash.clone()).or_default();

        // create block infos for missing pieces
        for p in downloaded_pieces.iter_zeros() {
            block_infos.extend(info.get_block_infos_of_piece_self(p));
        }

        info!("generated {} blocks", block_infos.len(),);

        self.endgame.insert(info_hash.clone(), false);

        let files_count = self
            .torrent_cache
            .get(info_hash)
            .ok_or(Error::TorrentDoesNotExist)?
            .file_metadata
            .len();

        self.block_cache.insert(info_hash.clone(), Default::default());

        self.dirty_files
            .insert(info_hash.clone(), Bitfield::from_piece(files_count));

        let piece_strategy =
            *self.piece_strategy.entry(info_hash.clone()).or_default();

        self.set_piece_strategy(info_hash, piece_strategy).await?;

        Ok(())
    }

    fn compute_torrent_cache(&mut self, info: &Info) {
        let mut file_metadata = Vec::new();
        let mut current_offset = 0;
        let base = self.base_path(&info.info_hash);

        if let Some(files) = &info.files {
            for file in files {
                let mut path = base.clone();
                path.extend(&file.path);
                file_metadata.push(FileMetadata {
                    path: path.into(),
                    start_offset: current_offset,
                    length: file.length,
                });
                current_offset += file.length;
            }
        } else {
            file_metadata.push(FileMetadata {
                path: base.into(),
                start_offset: 0,
                length: info.file_length.unwrap_or(0),
            });
        }

        self.torrent_cache.insert(
            info.info_hash.clone(),
            TorrentCache {
                file_metadata,
                piece_length: info.piece_length,
                total_size: info.get_torrent_size(),
            },
        );
    }

    /// Add a new peer to `peer_ctxs`.
    pub fn new_peer(&mut self, peer_ctx: Arc<PeerCtx>) {
        self.peer_ctxs.push(peer_ctx);
    }

    /// Add a new peer to `peer_ctxs`.
    pub fn delete_peer(&mut self, remote_addr: SocketAddr) {
        self.peer_ctxs.retain(|v| v.remote_addr != remote_addr);
    }

    /// Change the piece download algorithm to rarest-first.
    ///
    /// The rarest-first strategy actually begins in random-first,
    /// until the first piece is downloaded, after that, it finally
    /// switches to rarest-first.
    ///
    /// The function will get the pieces of all peers, and see
    /// which pieces are the most rare, and reorder the piece
    /// vector of Disk, where the most rare are the ones to the right.
    async fn rarest_first(
        &mut self,
        info_hash: &InfoHash,
    ) -> Result<(), Error> {
        // get all peers of the given torrent `info_hash`
        let peer_ctxs: Vec<Arc<PeerCtx>> = self
            .peer_ctxs
            .iter()
            .filter(|&v| v.torrent_ctx.info_hash == *info_hash)
            .cloned()
            .collect();

        debug!("calculating score of {:?} peers", peer_ctxs.len());

        if peer_ctxs.is_empty() {
            return Err(Error::NoPeers);
        }

        let info = self
            .torrent_info
            .get_mut(info_hash)
            .ok_or(Error::TorrentDoesNotExist)?;

        // vec of pieces scores/occurences, where index = piece.
        let score: Vec<AtomicUsize> =
            (0..info.pieces()).map(|_| AtomicUsize::new(0)).collect();

        let receivers: Vec<_> = peer_ctxs
            .par_iter()
            .filter_map(|ctx| {
                let (otx, orx) = oneshot::channel();
                let send_result = ctx
                    .torrent_ctx
                    .tx
                    .try_send(TorrentMsg::GetPeerBitfield(ctx.id.clone(), otx));

                match send_result {
                    Ok(()) => Some(orx),
                    Err(_) => None,
                }
            })
            .collect();

        let results = join_all(receivers).await;

        results.into_par_iter().for_each(|result| {
            if let Ok(Some(peer_pieces)) = result {
                for (i, item) in peer_pieces.iter().enumerate() {
                    if *item && i < score.len() {
                        score[i].fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        });

        // index, score
        let scored_pieces: Vec<(usize, usize)> = (0..info.pieces())
            .map(|i| (i, score[i].load(Ordering::Relaxed)))
            .collect();

        let order_map: HashMap<usize, usize> = scored_pieces
            .iter()
            .enumerate()
            .map(|(i, (piece_idx, _))| (*piece_idx, i))
            .collect();

        let infos = self
            .block_infos
            .get_mut(info_hash)
            .ok_or(Error::TorrentDoesNotExist)?;

        infos.sort_by_cached_key(|info| {
            order_map
                .get(&info.index)
                .copied()
                .unwrap_or(usize::MAX)
                .cmp(&info.begin)
        });

        let piece_strategy =
            self.piece_strategy.entry(info_hash.clone()).or_default();

        *piece_strategy = PieceStrategy::Rarest;

        Ok(())
    }

    fn mark_file_dirty(
        &mut self,
        info_hash: &InfoHash,
        file_index: usize,
    ) -> Result<(), Error> {
        let dirty_bits = self
            .dirty_files
            .get_mut(info_hash)
            .ok_or(Error::TorrentDoesNotExist)?;
        dirty_bits.safe_set(file_index);
        Ok(())
    }

    /// Return the file index given it's path on the torrent cache.
    fn get_file_index(
        &self,
        info_hash: &InfoHash,
        path: &Path,
    ) -> Option<usize> {
        self.torrent_cache.get(info_hash).and_then(|cache| {
            cache.file_metadata.iter().position(|meta| *meta.path == *path)
        })
    }

    /// Flush files in `self.dirty_files` to disk.
    async fn flush_dirty_files(&mut self) {
        for (info_hash, dirty_bits) in &mut self.dirty_files {
            // portion of `dirty_bits` succesfully flushed
            let mut clean_bits = Bitfield::from_piece(dirty_bits.len());
            let Some(cache) = self.torrent_cache.get(info_hash) else {
                continue;
            };
            for file_index in dirty_bits.iter_ones() {
                if let Some(file_meta) = cache.file_metadata.get(file_index)
                    && let Some(mmap) =
                        self.write_mmap_cache.get(&*file_meta.path)
                {
                    let mmap = mmap.lock().await;
                    // keep it marked as dirty if flush failed
                    if mmap.flush_async().is_ok() {
                        clean_bits.set(file_index, true);
                    }
                }
            }
            *dirty_bits &= !clean_bits;
        }
    }

    /// Request available metadata pieces.
    pub fn request_metadata(
        &mut self,
        info_hash: &InfoHash,
        qnt: usize,
    ) -> Result<Vec<MetadataPiece>, Error> {
        let metas = self
            .metadata_pieces
            .get_mut(info_hash)
            .ok_or(Error::TorrentDoesNotExist)?;

        if metas.is_empty() {
            return Ok(vec![]);
        }

        let v = metas.drain(0..qnt.min(metas.len())).collect();

        Ok(v)
    }

    async fn enter_endgame(
        &mut self,
        info_hash: InfoHash,
    ) -> Result<(), Error> {
        let endgame = self
            .endgame
            .get_mut(&info_hash)
            .ok_or(Error::TorrentDoesNotExist)?;

        if *endgame {
            return Ok(());
        }

        *endgame = true;

        let blocks = std::mem::take(
            self.block_infos
                .get_mut(&info_hash)
                .ok_or(Error::TorrentDoesNotExist)?,
        );

        let torrent_ctx = self
            .torrent_ctxs
            .get(&info_hash)
            .ok_or(Error::TorrentDoesNotExist)?;

        torrent_ctx.tx.send(TorrentMsg::Endgame(blocks)).await?;
        info!("endgame");

        Ok(())
    }

    /// Request available block infos following the order of PieceStrategy.
    pub async fn request_blocks(
        &mut self,
        peer_id: &PeerId,
        qnt: usize,
    ) -> Result<Vec<BlockInfo>, Error> {
        let Some(peer_ctx) = self.peer_ctxs.iter().find(|v| v.id == *peer_id)
        else {
            warn!("peer not found: {peer_id:?}");
            return Ok(vec![]);
        };

        let block_infos = self
            .block_infos
            .get_mut(&peer_ctx.torrent_ctx.info_hash)
            .ok_or(Error::TorrentDoesNotExist)?;

        if block_infos.is_empty() {
            self.tx
                .send(DiskMsg::Endgame(peer_ctx.torrent_ctx.info_hash.clone()))
                .await?;
            return Ok(vec![]);
        }

        let mut result = Vec::with_capacity(qnt);
        result.extend(block_infos.drain(0..qnt.min(block_infos.len())));

        Ok(result)
    }

    /// Open a file given a path.
    pub async fn open_file(path: impl AsRef<Path>) -> Result<File, Error> {
        let path = path.as_ref().to_owned();
        OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .await
            .map_err(|_| {
                Error::FileOpenError(path.to_str().unwrap().to_owned())
            })
    }

    /// Read the corresponding bytes of a block info from disk.
    pub async fn read_block(
        &mut self,
        info_hash: &InfoHash,
        block_info: &BlockInfo,
    ) -> Result<Bytes, Error> {
        let torrent_cache = self
            .torrent_cache
            .get(info_hash)
            .ok_or(Error::TorrentDoesNotExist)?;

        // calculate absolute offset
        let absolute_offset =
            block_info.index * torrent_cache.piece_length + block_info.begin;

        // find containing file
        let file_meta = torrent_cache
            .file_metadata
            .iter()
            .find(|m| {
                absolute_offset >= m.start_offset
                    && absolute_offset < m.start_offset + m.length
            })
            .ok_or(Error::TorrentDoesNotExist)?;

        // calculate file-relative offset
        let start = absolute_offset - file_meta.start_offset;
        let end = start + block_info.len;
        let path = &file_meta.path.clone();

        // get cached file handle
        let mmap = self.get_cached_read_mmap(path).await?;

        // Return a Bytes object that references the memory-mapped data
        Ok(Bytes::copy_from_slice(&mmap[start..end]))
    }

    /// Write block to a cache of pieces, when a piece has been fully
    /// downloaded and validated, write it to disk and clear the cache.
    ///
    /// If the download algorithm of the pieces is "Random", and it has
    /// downloaded the first piece, change the algorithm to rarest-first.
    pub async fn write_block(
        &mut self,
        info_hash: &InfoHash,
        block: Block,
    ) -> Result<(), Error> {
        // Write the block's data to the correct position in the file
        let piece_size = self.piece_size(info_hash, block.index)?;

        let torrent_ctx = self
            .torrent_ctxs
            .get(info_hash)
            .ok_or(Error::TorrentDoesNotExist)?
            .clone();

        let (written, cache) = self
            .block_cache
            .get_mut(info_hash)
            .ok_or(Error::TorrentDoesNotExist)?
            .entry(block.index)
            // if there is no index this means that this is a duplicate block
            .or_default();

        if cache.len() < piece_size {
            cache.resize(piece_size, 0);
        }
        *written += block.block.len();
        cache[block.begin..].copy_from_slice(&block.block);

        // piece not fully downloaded, return
        if *written < self.piece_size(info_hash, block.index)? {
            return Ok(());
        }

        // piece IS fully downloaded, verify and write

        // if the piece is corrupted, generate block infos
        if self.validate_piece(info_hash, block.index).await.is_err() {
            let info = self
                .torrent_cache
                .get(info_hash)
                .ok_or(Error::TorrentDoesNotExist)?;

            let info_blocks = Info::get_block_infos_of_piece(
                info.total_size,
                info.piece_length,
                block.index,
            );

            warn!(
                "piece {} is corrupted, generating more {} block infos",
                block.index,
                info_blocks.len()
            );

            self.block_infos
                .get_mut(info_hash)
                .ok_or(Error::TorrentDoesNotExist)?
                .extend(info_blocks);

            return Ok(());
        }

        let downloaded_pieces_len = self
            .downloaded_pieces
            .get_mut(info_hash)
            .ok_or(Error::TorrentDoesNotExist)?;

        *downloaded_pieces_len += 1;

        debug!("piece {} is valid.", block.index);

        let _ =
            torrent_ctx.tx.send(TorrentMsg::DownloadedPiece(block.index)).await;

        if *self
            .piece_strategy
            .get(info_hash)
            .ok_or(Error::TorrentDoesNotExist)?
            == PieceStrategy::Random
            && *downloaded_pieces_len == 1
        {
            debug!(
                "first piece downloaded, and piece order is random, switching
                  to rarest-first"
            );
            self.set_piece_strategy(info_hash, PieceStrategy::Rarest).await?;
        }

        // write the piece to disk
        self.write_piece(info_hash, block.index).await?;

        Ok(())
    }

    #[allow(clippy::type_complexity)]
    fn calculate_write_ops(
        &self,
        info_hash: &InfoHash,
        piece_index: usize,
        piece_buffer: &[u8],
    ) -> Result<Vec<(Arc<Path>, usize, std::ops::Range<usize>)>, Error> {
        let cache = self
            .torrent_cache
            .get(info_hash)
            .ok_or(Error::TorrentDoesNotExist)?;

        let piece_start = piece_index * cache.piece_length;
        let piece_end =
            (piece_start + cache.piece_length).min(cache.total_size);
        let piece_size = piece_end - piece_start;

        // Validate piece buffer size
        if piece_buffer.len() < piece_size {
            return Err(Error::PieceInvalid);
        }

        let mut write_ops: Vec<(Arc<Path>, usize, std::ops::Range<usize>)> =
            Vec::with_capacity(cache.file_metadata.len());

        for file_meta in &cache.file_metadata {
            let file_end = file_meta.start_offset + file_meta.length;
            let overlap_start = piece_start.max(file_meta.start_offset);
            let overlap_end = piece_end.min(file_end);

            if overlap_start >= overlap_end {
                continue;
            }

            let buffer_start = overlap_start - piece_start;
            let buffer_end = overlap_end - piece_start;
            let file_offset = overlap_start - file_meta.start_offset;

            write_ops.push((
                file_meta.path.clone(),
                file_offset,
                buffer_start..buffer_end,
            ));
        }

        Ok(write_ops)
    }

    /// Write all cached blocks of `piece` to disk.
    /// It will free the blocks in the cache.
    pub async fn write_piece(
        &mut self,
        info_hash: &InfoHash,
        piece: usize,
    ) -> Result<(), Error> {
        let piece_buffer = self
            .block_cache
            .get_mut(info_hash)
            .ok_or(Error::TorrentDoesNotExist)?
            .remove(&piece)
            .ok_or(Error::PieceInvalid)?
            .1;

        // calculate write operations
        let write_ops =
            self.calculate_write_ops(info_hash, piece, &piece_buffer)?;

        // group writes by file
        let mut file_ops: HashMap<
            Arc<Path>,
            Vec<(usize, std::ops::Range<usize>)>,
        > = HashMap::with_capacity(write_ops.len());

        for (path, file_offset, data_range) in write_ops {
            file_ops
                .entry(path.clone())
                .or_default()
                .push((file_offset, data_range));
        }

        for (path, ops) in file_ops {
            let mmap_arc = self.get_cached_write_mmap(&path).await?;
            let mut mmap = mmap_arc.lock().await;

            // sort ops by offset for sequential write
            let mut ops = ops;
            ops.sort_by_key(|(offset, _)| *offset);

            for (file_offset, data_range) in ops {
                let start = file_offset;
                let end = start + data_range.len();

                // if mmap.len() < end {
                //     mmap.copy_from_slice(&vec![0; end]);
                // }

                mmap[start..end].copy_from_slice(&piece_buffer[data_range]);
            }

            let _ = self.mark_file_dirty(
                info_hash,
                self.get_file_index(info_hash, &path).unwrap(),
            );
        }

        Ok(())
    }

    /// Get the correct piece size, the last piece of a torrent
    /// might be smaller than the other pieces.
    fn piece_size(
        &self,
        info_hash: &InfoHash,
        piece_index: usize,
    ) -> Result<usize, Error> {
        let info = self
            .torrent_info
            .get(info_hash)
            .ok_or(Error::TorrentDoesNotExist)?;

        Ok(if piece_index == info.pieces() - 1 {
            let remainder = info.get_torrent_size() % info.piece_length;
            if remainder == 0 { info.piece_length } else { remainder }
        } else {
            info.piece_length
        })
    }

    /// Get the base path of a torrent directory.
    /// Which is always "download_dir/name_of_torrent".
    pub fn base_path(&self, info_hash: &InfoHash) -> PathBuf {
        let info = self.torrent_info.get(info_hash).unwrap();
        let mut base = self.config.download_dir.clone();
        base.push(&info.name);
        base
    }

    async fn get_cached_read_mmap(
        &mut self,
        path: &Arc<Path>,
    ) -> Result<Arc<Mmap>, Error> {
        *self.last_accessed.entry(path.clone()).or_insert(Instant::now()) =
            Instant::now();

        if let Some(mmap) = self.read_mmap_cache.get(path) {
            return Ok(mmap.clone());
        }

        // cache miss
        let file = Self::open_file(path).await?;
        let mmap = unsafe { Mmap::map(&file)? };
        let mmap = Arc::new(mmap);

        self.read_mmap_cache.put(path.clone(), mmap.clone());
        Ok(mmap)
    }

    async fn get_cached_write_mmap(
        &mut self,
        path: &Arc<Path>,
    ) -> Result<Arc<Mutex<MmapMut>>, Error> {
        *self.last_accessed.entry(path.clone()).or_insert(Instant::now()) =
            Instant::now();

        if let Some(mmap) = self.write_mmap_cache.get(path) {
            return Ok(mmap.clone());
        }

        // cache miss
        let file = Self::open_file(path).await?;
        let mmap = unsafe { MmapMut::map_mut(&file) }?;
        let mmap = Arc::new(Mutex::new(mmap));

        self.write_mmap_cache.put(path.clone(), mmap.clone());
        Ok(mmap)
    }

    async fn pre_alloc_files(
        &mut self,
        info_hash: &InfoHash,
    ) -> Result<(), Error> {
        if let Some(cache) = self.torrent_cache.get(info_hash) {
            for meta in &cache.file_metadata {
                if let Some(parent) = meta.path.parent() {
                    tokio::fs::create_dir_all(parent).await?;
                }

                if let Ok(file) = Self::open_file(&meta.path).await {
                    file.set_len(meta.length as u64).await?;
                }
            }
        }
        Ok(())
    }

    #[inline]
    fn complete_torrents_path(&self) -> PathBuf {
        let mut path = self.config.metadata_dir.clone();
        path.push("complete");
        path
    }

    #[inline]
    fn incomplete_torrents_path(&self) -> PathBuf {
        let mut path = self.config.metadata_dir.clone();
        path.push("incomplete");
        path
    }

    #[inline]
    fn queue_torrents_path(&self) -> PathBuf {
        let mut path = self.config.metadata_dir.clone();
        path.push("queue");
        path
    }

    /// Write an incomplete torrent info to disk
    async fn write_incomplete_torrent_metainfo(
        &self,
        info: Info,
    ) -> Result<(), Error> {
        let ctx = self
            .torrent_ctxs
            .get(&info.info_hash)
            .ok_or(Error::TorrentDoesNotExist)?;

        let (otx, orx) = oneshot::channel();
        ctx.tx.send(TorrentMsg::GetAnnounceList(otx)).await?;
        let announce_list = orx.await?;

        let mut path = self.incomplete_torrents_path();
        path.push(&info.name);
        path.set_extension("torrent");

        let metainfo: MetaInfo = info.to_meta_info(&announce_list);

        let buff = metainfo.to_bencode()?;
        let mut file = Self::open_file(path).await?;
        file.write_all(&buff).await?;

        Ok(())
    }

    /// Move the torrent metainfo to the torrents/complete folder
    async fn write_complete_torrent_metainfo(
        &self,
        info: &InfoHash,
    ) -> Result<(), Error> {
        let info =
            self.torrent_info.get(info).ok_or(Error::TorrentDoesNotExist)?;

        let mut incomplete_file_path = self.incomplete_torrents_path();
        incomplete_file_path.push(&info.name);
        incomplete_file_path.set_extension("torrent");

        let mut complete_file_path = self.complete_torrents_path();
        complete_file_path.push(&info.name);
        complete_file_path.set_extension("torrent");

        let mut buf = Vec::with_capacity(info.metadata_size);
        let mut file = Self::open_file(&incomplete_file_path).await?;
        file.read_to_end(&mut buf).await?;

        tokio::fs::remove_file(incomplete_file_path).await?;

        let mut file = Self::open_file(complete_file_path).await?;
        file.write_all(&buf).await?;

        Ok(())
    }

    fn is_torrent_complete(&self, name: &str) -> bool {
        let mut path = self.complete_torrents_path();
        path.push(name);
        path.set_extension("torrent");
        path.exists()
    }

    fn is_torrent_incomplete(&self, name: &str) -> bool {
        let mut path = self.incomplete_torrents_path();
        path.push(name);
        path.set_extension("torrent");
        path.exists()
    }

    /// Read all .torrent files from (in)complete folder, and add them to the
    /// client.
    pub async fn read_metainfos_and_add(
        &mut self,
        metadata_dir: MetadataDir,
    ) -> Result<(), Error> {
        // ~/.config/vincenzo/torrents/incomplete
        let path = match metadata_dir {
            MetadataDir::Queue => self.queue_torrents_path(),
            MetadataDir::Incomplete => self.incomplete_torrents_path(),
            MetadataDir::Complete => self.complete_torrents_path(),
        };
        let mut entries = tokio::fs::read_dir(path).await?;

        while let Some(entry) = entries.next_entry().await? {
            // ~/.config/vincenzo/torrents/incomplete/file.torrent
            let path = entry.path();

            // only read .torrent files
            if path.extension().and_then(|s| s.to_str()) != Some("torrent") {
                continue;
            }

            let bytes = tokio::fs::read(&path).await?;
            let metainfo = MetaInfo::from_bencode(&bytes)?;

            // skip duplicate
            if self.torrent_ctxs.contains_key(&metainfo.info.info_hash) {
                continue;
            }

            self.new_torrent_metainfo(metainfo).await?;
        }

        Ok(())
    }

    fn verify_piece(
        piece_index: usize,
        mmaps: &[(&FileMetadata, Arc<Mmap>)],
        piece_length: usize,
        total_size: usize,
        expected_hash: [u8; 20],
    ) -> bool {
        let piece_start = piece_index * piece_length;
        let piece_end = std::cmp::min(piece_start + piece_length, total_size);
        let piece_size = piece_end - piece_start;

        let mut hasher = Sha1::new();
        let mut bytes_remaining = piece_size;

        for (file_meta, mmap) in mmaps {
            let file_start = file_meta.start_offset;
            let file_end = file_start + file_meta.length;

            // check if this file overlaps with the piece
            if piece_start < file_end && piece_end > file_start {
                let read_start = piece_start.max(file_start);
                let read_end = piece_end.min(file_end);
                let read_length = read_end - read_start;

                // calculate file offset
                let file_offset = read_start - file_start;

                // handle out-of-bounds access
                // the file is too small to be read from, the torrent is likely
                // incomplete.
                if file_offset >= mmap.len() {
                    continue;
                }

                // calculate how much we can read from the file
                let available_bytes = read_length.min(mmap.len() - file_offset);

                // read available bytes from memory map
                if available_bytes > 0 {
                    let data =
                        &mmap[file_offset..file_offset + available_bytes];
                    hasher.update(data);
                }

                bytes_remaining -= read_length;
            }

            if bytes_remaining == 0 {
                break;
            }
        }

        let hash = &hasher.finalize()[..];
        hash == expected_hash
    }

    /// Validate if the hash of a piece is valid.
    ///
    /// # Important
    /// The function will get the blocks in cache,
    /// if the cache was cleared, the function will not work.
    #[tracing::instrument(skip(self, info_hash))]
    pub async fn validate_piece(
        &mut self,
        info_hash: &InfoHash,
        index: usize,
    ) -> Result<(), Error> {
        let b = index * 20;
        let e = b + 20;

        let info = self
            .torrent_info
            .get(info_hash)
            .ok_or(Error::TorrentDoesNotExist)?;

        let hash_from_info = info.pieces[b..e].to_owned();

        let buf = &self
            .block_cache
            .get_mut(info_hash)
            .ok_or(Error::TorrentDoesNotExist)?
            .get_mut(&index)
            .ok_or(Error::TorrentDoesNotExist)?
            .1;

        let mut hasher = Sha1::new();
        hasher.update(buf);

        let hash = &hasher.finalize()[..];

        if *hash_from_info.as_slice() != *hash {
            return Err(Error::PieceInvalid);
        }

        Ok(())
    }

    fn get_metainfo_path(
        &self,
        info_hash: &InfoHash,
    ) -> Result<PathBuf, Error> {
        let info = self
            .torrent_info
            .get(info_hash)
            .ok_or(Error::TorrentDoesNotExist)?;

        if self.is_torrent_complete(&info.name) {
            let mut path = self.complete_torrents_path();
            path.push(&info.name);
            path.set_extension("torrent");
            return Ok(path);
        } else if self.is_torrent_incomplete(&info.name) {
            let mut path = self.incomplete_torrents_path();
            path.push(&info.name);
            path.set_extension("torrent");
            return Ok(path);
        }
        Err(Error::TorrentDoesNotExist)
    }

    pub async fn set_piece_strategy(
        &mut self,
        info_hash: &InfoHash,
        s: PieceStrategy,
    ) -> Result<(), Error> {
        let current = self.piece_strategy.entry(info_hash.clone()).or_default();
        *current = s;
        let infos = self
            .block_infos
            .get_mut(info_hash)
            .ok_or(Error::TorrentDoesNotExist)?;

        match s {
            PieceStrategy::Random => {
                infos.shuffle(&mut rand::rng());
            }
            PieceStrategy::Sequential => {
                infos.sort();
            }
            PieceStrategy::Rarest => {
                self.rarest_first(info_hash).await?;
            }
        }

        Ok(())
    }
}
