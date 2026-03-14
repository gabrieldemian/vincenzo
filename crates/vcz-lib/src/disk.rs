//! Disk is responsible for file I/O info_hash, pieces all torrents.

// extern crate page_size;
use crate::{
    bitfield::{Bitfield, VczBitfield},
    config::ResolvedConfig,
    daemon::{DaemonCtx, DaemonMsg},
    error::Error,
    extensions::{
        BLOCK_LEN, MetadataPiece,
        core::{Block, BlockInfo},
    },
    metainfo::{Info, InfoHash, MetaInfo},
    peer::PeerCtx,
    torrent::{
        Torrent, TorrentCtx, TorrentMsg, TorrentStatus, TorrentStatusErrorCode,
    },
};
use bendy::decoding::FromBencode;
use bytes::Bytes;
use memmap2::{Mmap, MmapMut, UncheckedAdvice};
use page_size;
use quick_cache::sync::Cache;
use rand::seq::SliceRandom;
use rayon::iter::{IntoParallelIterator, ParallelIterator};
use sha1::{Digest, Sha1};
use std::{
    collections::{BTreeMap, HashMap},
    fs::{File, OpenOptions},
    io::ErrorKind,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, RwLock, atomic::Ordering},
    time::Duration,
};
use tokio::{
    select,
    sync::{
        mpsc::{self, Receiver},
        oneshot::Sender,
    },
    time::interval,
};
use tracing::{debug, error, info, warn};

/// Cache capacity of files.
const FILE_CACHE_CAPACITY: usize = 512;

/// In millis
const COLD_PIECE_THRESHOLD: u64 = 30_000;

/// There are 3 dirs inside `torrents` to store metainfo files in their
/// contexts.
#[derive(Clone, Copy, PartialEq)]
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

    DeleteTorrent(InfoHash, bool),

    FinishedDownload(InfoHash),

    ReadBlock {
        info_hash: InfoHash,
        block_info: BlockInfo,
        recipient: Sender<Result<Bytes, Error>>,
    },

    /// Write the given block to disk, the Disk struct will get the seeked file
    /// automatically.
    WriteBlock {
        ctx: Arc<TorrentCtx>,
        block: Block,
    },

    /// Request block infos that the peer has, that we do not have ir nor
    /// requested it.
    RequestBlocks {
        peer_ctx: Arc<PeerCtx>,
        pieces: Vec<usize>,
        recipient: Sender<Result<Vec<BlockInfo>, Error>>,
        bitfield: Box<Bitfield>,
    },

    /// Request a piece of the metadata.
    RequestMetadata {
        info_hash: InfoHash,
        recipient: Sender<Vec<MetadataPiece>>,
        metadata_size: usize,
        qnt: usize,
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
    Random,
    /// Rarest-first, give priority to the rarest pieces.
    Rarest,
    /// Sequential downloads, useful in streaming.
    #[default]
    Sequential,
}

// Precomputed file metadata for fast lookups
#[derive(Debug, Clone)]
pub(crate) struct FileMetadata {
    pub path: Arc<Path>,
    pub start_offset: usize,
    pub length: usize,
}

// Cache entry with precomputed metadata
#[derive(Debug, Clone)]
pub(crate) struct TorrentCache {
    pub file_metadata: Vec<FileMetadata>,
    pub piece_length: usize,
    pub num_pieces: usize,
    pub total_size: usize,
}

impl TorrentCache {
    #[inline]
    pub(crate) fn piece_length(&self, piece: usize) -> usize {
        if piece == self.num_pieces - 1 {
            let rem = self.total_size % self.piece_length;
            if rem == 0 { self.piece_length } else { rem }
        } else {
            self.piece_length
        }
    }
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
/// - Validate hash of pieces
/// - Answer messages
pub struct Disk {
    pub config: Arc<ResolvedConfig>,
    pub tx: mpsc::Sender<DiskMsg>,
    pub(crate) daemon_ctx: Arc<DaemonCtx>,
    pub(crate) rx: Receiver<DiskMsg>,
    pub(crate) free_rx: mpsc::UnboundedReceiver<ReturnToDisk>,
    pub torrent_ctxs: HashMap<InfoHash, Arc<TorrentCtx>>,
    pub(crate) piece_strategy: HashMap<InfoHash, PieceStrategy>,

    /// How many pieces were downloaded.
    pub(crate) downloaded_pieces_len: HashMap<InfoHash, u64>,
    pub(crate) requested_pieces_len: HashMap<InfoHash, usize>,
    pub(crate) endgame: HashMap<InfoHash, bool>,

    // A cache of blocks, where the key is a piece. The cache will be cleared
    // when the entire piece is downloaded and validated as it will be
    // written to disk.
    pub(crate) block_cache: HashMap<InfoHash, BTreeMap<usize, Vec<Block>>>,

    pub(crate) torrent_info: HashMap<InfoHash, Arc<Info>>,

    /// Block infos returned by peers.
    pub(crate) queue: HashMap<InfoHash, Vec<BlockInfo>>,

    pub(crate) metadata_pieces: HashMap<InfoHash, Vec<MetadataPiece>>,

    /// A cache of torrent files with pre-computed lengths.
    pub(crate) torrent_cache: HashMap<InfoHash, Arc<TorrentCache>>,

    pub(crate) piece_tracker: Arc<RwLock<HashMap<InfoHash, PieceTracker>>>,
    pub(crate) read_mmap_cache: Cache<Arc<Path>, Arc<Mmap>>,
    pub(crate) write_mmap_cache: Cache<Arc<Path>, Arc<Mutex<MmapMut>>>,
}

pub struct PieceTracker {
    last_accessed: Box<[u64]>,
}

impl PieceTracker {
    fn record(&mut self, piece: usize) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        self.last_accessed[piece] = now;
    }
}

impl Disk {
    pub fn new(
        config: Arc<ResolvedConfig>,
        daemon_ctx: Arc<DaemonCtx>,
        tx: mpsc::Sender<DiskMsg>,
        rx: mpsc::Receiver<DiskMsg>,
        free_rx: mpsc::UnboundedReceiver<ReturnToDisk>,
    ) -> Self {
        let read_mmap_cache = Cache::with_weighter(
            FILE_CACHE_CAPACITY,
            FILE_CACHE_CAPACITY as u64,
            quick_cache::UnitWeighter,
        );
        let write_mmap_cache = Cache::with_weighter(
            FILE_CACHE_CAPACITY,
            FILE_CACHE_CAPACITY as u64,
            quick_cache::UnitWeighter,
        );
        Self {
            config,
            daemon_ctx,
            free_rx,
            rx,
            tx,
            read_mmap_cache,
            write_mmap_cache,
            torrent_cache: HashMap::new(),
            piece_tracker: Arc::new(RwLock::new(HashMap::new())),
            metadata_pieces: HashMap::new(),
            block_cache: HashMap::new(),
            torrent_ctxs: HashMap::new(),
            downloaded_pieces_len: HashMap::new(),
            requested_pieces_len: HashMap::new(),
            endgame: HashMap::new(),
            piece_strategy: HashMap::default(),
            queue: HashMap::default(),
            torrent_info: HashMap::default(),
        }
    }

    #[tracing::instrument(name = "disk", skip_all)]
    pub async fn run(&mut self) -> Result<(), Error> {
        let mut heartbeat_interval = interval(Duration::from_millis(1_000));
        let mut evict_interval =
            interval(Duration::from_millis(COLD_PIECE_THRESHOLD / 3));

        // ensure the necessary folders are created.
        #[cfg(not(feature = "ui-test"))]
        {
            tokio::fs::create_dir_all(self.incomplete_torrents_path()).await?;
            tokio::fs::create_dir_all(self.complete_torrents_path()).await?;
            tokio::fs::create_dir_all(self.queue_torrents_path()).await?;
        }

        // load .torrent files into the client.
        #[cfg(not(feature = "ui-test"))]
        {
            self.read_metainfos_and_add(MetadataDir::Incomplete).await?;
            self.read_metainfos_and_add(MetadataDir::Complete).await?;
        }

        'outer: loop {
            select! {
                Some(msg) = self.rx.recv() => {
                    match msg {
                        DiskMsg::DeleteTorrent(info_hash, also_from_disk) => {
                            let metainfo_path = self.get_metainfo_path(&info_hash)?;
                            let Some(files_path) = self.torrent_cache.get(&info_hash)
                                else { continue };
                            let files_path = &files_path.file_metadata[0].path;

                            info!("metainfo {metainfo_path:?}");
                            info!("disk path {files_path:?}");

                            tokio::fs::remove_file(metainfo_path).await?;

                            if also_from_disk {
                                if files_path.is_file() {
                                    tokio::fs::remove_file(files_path).await?;
                                } else {
                                    tokio::fs::remove_dir_all(files_path).await?;
                                }
                            }

                            self.block_cache.remove_entry(&info_hash);
                            self.downloaded_pieces_len.remove_entry(&info_hash);
                            self.endgame.remove_entry(&info_hash);
                            self.metadata_pieces.remove_entry(&info_hash);
                            self.piece_strategy.remove_entry(&info_hash);
                            self.torrent_cache.remove_entry(&info_hash);
                            self.torrent_ctxs.remove_entry(&info_hash);
                            self.torrent_info.remove_entry(&info_hash);
                        }
                        DiskMsg::FinishedDownload(info_hash) => {
                            self.block_cache.remove_entry(&info_hash);
                            self.downloaded_pieces_len.remove_entry(&info_hash);
                            self.endgame.remove_entry(&info_hash);
                            self.metadata_pieces.remove_entry(&info_hash);
                            self.piece_strategy.remove_entry(&info_hash);
                            self.queue.remove_entry(&info_hash);
                            self.torrent_ctxs.remove_entry(&info_hash);
                            self.torrent_info.remove_entry(&info_hash);
                            let _ = self.move_metainfo_to_dir(
                                &info_hash,
                                MetadataDir::Incomplete,
                                MetadataDir::Complete
                            ).await;
                            self.sync_torrent(&info_hash).await?;
                        }
                        DiskMsg::AddTorrent(torrent, info) => {
                            let _ = self.add_torrent(torrent, info).await;
                        }
                        DiskMsg::ReadBlock { block_info, recipient, info_hash } => {
                            let r = self.read_block(&info_hash, &block_info);
                            let _ = recipient.send(r);
                        }
                        DiskMsg::WriteBlock { block, ctx } => {
                            let r = self.write_block(ctx.clone(), block).await;
                            if let Err(e) = &r {
                                self.handle_torrent_err(ctx, e).await;
                            }
                        }
                        DiskMsg::RequestBlocks {  recipient, peer_ctx, pieces, bitfield } => {
                            let infos = self
                                .request_blocks(peer_ctx, pieces, bitfield)
                                .await;
                            let _ = recipient.send(infos);
                        }
                        DiskMsg::RequestMetadata { qnt, recipient, info_hash, metadata_size } => {
                            let pieces = self
                                .request_metadata(&info_hash, qnt, metadata_size)
                                .unwrap_or_default();

                            let _ = recipient.send(pieces);
                        }
                        DiskMsg::Quit => {
                            debug!("Quit");
                            break 'outer Ok(());
                        }
                    }
                }
                _ = heartbeat_interval.tick() => {
                    let _ = self.read_metainfos_and_add(MetadataDir::Queue).await;
                }
                _ = evict_interval.tick() => {
                    let _ = self.evict_cold_pieces();
                }
                Some(return_to_disk) = self.free_rx.recv() => {
                    match return_to_disk {
                        ReturnToDisk::Block(info_hash, blocks) => {
                            let infos = self.queue
                                .get_mut(&info_hash)
                                .ok_or(Error::TorrentDoesNotExist)?;
                            infos.extend(blocks);
                        }
                        ReturnToDisk::Metadata(info_hash, pieces) => {
                            self.metadata_pieces
                                .get_mut(&info_hash)
                                .ok_or(Error::TorrentDoesNotExist)?
                                .extend(pieces);
                        }
                    };
                }
            }
        }
    }

    async fn handle_torrent_err(&self, ctx: Arc<TorrentCtx>, err: &Error) {
        if let Error::IO(e) = err {
            match e.kind() {
                ErrorKind::NotFound => {
                    let _ = ctx
                        .tx
                        .send(TorrentMsg::SetTorrentError(
                            TorrentStatusErrorCode::FilesMissing,
                        ))
                        .await;
                }
                _ => {
                    let _ = ctx
                        .tx
                        .send(TorrentMsg::SetTorrentError(
                            TorrentStatusErrorCode::Unknown,
                        ))
                        .await;
                }
            }
        };
    }

    /// Create a new torrent from a [`MetaInfo`] and send a
    /// [`DaemonMsg::AddTorrentMetaInfo`].
    #[tracing::instrument(skip_all, fields(name = %metainfo.info.name))]
    pub async fn new_torrent_metainfo(
        &mut self,
        metainfo: MetaInfo,
        metadata_dir: MetadataDir,
    ) -> Result<(), Error> {
        let info_hash = metainfo.info.info_hash.clone();
        let info = Arc::new(metainfo.info.clone());

        self.torrent_info.insert(info_hash.clone(), info);

        self.compute_torrent_cache(&metainfo.info);

        if metadata_dir == MetadataDir::Queue {
            // create folders, files, and preallocate them with zeroes.
            self.pre_alloc_files(&info_hash).await?;
        }

        let mut is_err = false;
        let downloaded_pieces =
            match self.compute_downloaded_pieces(&metainfo.info).await {
                Ok(downloaded_pieces) => downloaded_pieces,
                Err(Error::TorrentFilesMissing(downloaded_pieces)) => {
                    is_err = true;
                    downloaded_pieces
                }
                Err(e) => return Err(e),
            };

        self.compute_torrent_state(&info_hash, &downloaded_pieces).await?;

        let dp = downloaded_pieces.count_ones();
        let is_complete = dp >= metainfo.info.pieces();
        let info_hash = metainfo.info.info_hash.clone();

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

        if is_complete {
            self.move_metainfo_to_dir(
                &info_hash,
                MetadataDir::Incomplete,
                MetadataDir::Complete,
            )
            .await?;
        }

        self.torrent_ctxs.insert(info_hash.clone(), torrent.ctx.clone());
        self.daemon_ctx
            .tx
            .send(DaemonMsg::AddTorrentMetaInfo(Box::new(torrent)))
            .await?;

        Ok(())
    }

    fn preopen_files<'a>(
        &self,
        file_metadata: &'a [FileMetadata],
    ) -> Result<
        Vec<(&'a FileMetadata, Arc<Mmap>)>,
        Vec<(&'a FileMetadata, Arc<Mmap>)>,
    > {
        let mut mmaps = Vec::with_capacity(file_metadata.len());

        for file_meta in file_metadata {
            let path = file_meta.path.as_ref().to_owned();
            // return error if the file doesn't exist
            if let Ok(file) = Self::open_file(path) {
                let mmap = unsafe { Mmap::map(&file).unwrap() };
                mmaps.push((file_meta, Arc::new(mmap)));
            } else {
                return Err(mmaps);
            };
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
        let file_handles = self.preopen_files(&torrent_cache.file_metadata);

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
            unsafe {
                if let Err(e) = mmap.unchecked_advise(UncheckedAdvice::DontNeed)
                {
                    panic!("Failed to advise DontNeed on mmap: {}", e);
                }
            }
            self.read_mmap_cache.insert(f.path.clone(), mmap);
        }

        let mut downloaded_pieces = Bitfield::from_piece(total_pieces);

        for (piece_index, result) in piece_results.into_iter().enumerate() {
            downloaded_pieces.set(piece_index, result);
        }

        tracing::debug!(
            "computed {} pieces, {} downloaded",
            downloaded_pieces.len(),
            downloaded_pieces.count_ones()
        );

        if is_err {
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
        let downloaded_count = downloaded_pieces.count_ones();

        self.downloaded_pieces_len
            .insert(info_hash.clone(), downloaded_count as u64);
        self.requested_pieces_len.insert(info_hash.clone(), downloaded_count);
        self.block_cache.insert(info_hash.clone(), Default::default());
        self.endgame.insert(info_hash.clone(), false);
        self.queue.insert(info_hash.clone(), Vec::new());

        let piece_strategy =
            *self.piece_strategy.entry(info_hash.clone()).or_default();

        self.set_piece_strategy(info_hash, piece_strategy)?;

        Ok(())
    }

    #[inline]
    fn compute_torrent_cache(&mut self, info: &Info) {
        let mut file_metadata = Vec::new();
        let mut current_offset = 0;
        let base = self.base_path(&info.info_hash);
        let num_pieces = info.pieces();

        let mut lock = self
            .piece_tracker
            .write()
            .expect("lock piece_tracker compute_torrent_cache");

        let last_accessed = vec![0; num_pieces].into_boxed_slice();
        lock.insert(info.info_hash.clone(), PieceTracker { last_accessed });

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
            Arc::new(TorrentCache {
                file_metadata,
                piece_length: info.piece_length,
                total_size: info.get_torrent_size(),
                num_pieces,
            }),
        );
    }

    // Change the piece download algorithm to rarest-first.
    //
    // The rarest-first strategy actually begins in random-first,
    // until the first piece is downloaded, after that, it finally
    // switches to rarest-first.
    //
    // The function will get the pieces of all peers, and see
    // which pieces are the most rare, and reorder the piece
    // vector of Disk, where the most rare are the ones to the right.
    // async fn rarest_first(
    //     &mut self,
    //     info_hash: &InfoHash,
    // ) -> Result<(), Error> {
    //     // get all peers of the given torrent `info_hash`
    //     let peer_ctxs: Vec<Arc<PeerCtx>> = self
    //         .peer_ctxs
    //         .iter()
    //         .filter(|&v| v.torrent_ctx.info_hash == *info_hash)
    //         .cloned()
    //         .collect();
    //
    //     debug!("calculating score of {:?} peers", peer_ctxs.len());
    //
    //     if peer_ctxs.is_empty() {
    //         return Err(Error::NoPeers);
    //     }
    //
    //     let info = self
    //         .torrent_info
    //         .get_mut(info_hash)
    //         .ok_or(Error::TorrentDoesNotExist)?;
    //
    //     // vec of pieces scores/occurences, where index = piece.
    //     let score: Vec<AtomicUsize> =
    //         (0..info.pieces()).map(|_| AtomicUsize::new(0)).collect();
    //
    //     let receivers: Vec<_> = peer_ctxs
    //         .par_iter()
    //         .filter_map(|ctx| {
    //             let (otx, orx) = oneshot::channel();
    //             let send_result = ctx
    //                 .torrent_ctx
    //                 .tx
    //                 .try_send(TorrentMsg::GetPeerBitfield(ctx.id.clone(),
    // otx));
    //
    //             match send_result {
    //                 Ok(()) => Some(orx),
    //                 Err(_) => None,
    //             }
    //         })
    //         .collect();
    //
    //     let results = join_all(receivers).await;
    //
    //     results.into_par_iter().for_each(|result| {
    //         if let Ok(Some(peer_pieces)) = result {
    //             for (i, item) in peer_pieces.iter().enumerate() {
    //                 if *item && i < score.len() {
    //                     score[i].fetch_add(1, Ordering::Relaxed);
    //                 }
    //             }
    //         }
    //     });
    //
    //     // index, score
    //     let scored_pieces: Vec<(usize, usize)> = (0..info.pieces())
    //         .map(|i| (i, score[i].load(Ordering::Relaxed)))
    //         .collect();
    //
    //     let order_map: HashMap<usize, usize> = scored_pieces
    //         .iter()
    //         .enumerate()
    //         .map(|(i, (piece_idx, _))| (*piece_idx, i))
    //         .collect();
    //
    //     let infos = self
    //         .block_infos
    //         .get_mut(info_hash)
    //         .ok_or(Error::TorrentDoesNotExist)?;
    //
    //     infos.sort_by_cached_key(|info| {
    //         order_map
    //             .get(&info.index)
    //             .copied()
    //             .unwrap_or(usize::MAX)
    //             .cmp(&info.begin)
    //     });
    //
    //     let piece_strategy =
    //         self.piece_strategy.entry(info_hash.clone()).or_default();
    //
    //     *piece_strategy = PieceStrategy::Rarest;
    //
    //     Ok(())
    // }

    /// Request available block infos following the order of PieceStrategy.
    #[inline]
    pub async fn request_blocks(
        &mut self,
        peer_ctx: Arc<PeerCtx>,
        pieces: Vec<usize>,
        mut bitfield: Box<Bitfield>,
    ) -> Result<Vec<BlockInfo>, Error> {
        let info_hash = &peer_ctx.torrent_ctx.info_hash;

        let info = self
            .torrent_cache
            .get(info_hash)
            .ok_or(Error::TorrentDoesNotExist)?;

        let req = self
            .requested_pieces_len
            .get_mut(info_hash)
            .ok_or(Error::TorrentDoesNotExist)?;

        let per_piece = info.piece_length.div_ceil(BLOCK_LEN);
        let mut result = Vec::with_capacity(per_piece * pieces.len());

        for p in pieces {
            result.extend(Info::get_block_infos_of_piece(
                info.total_size,
                info.piece_length,
                p,
            ));
            *req += 1;
        }

        let queue =
            self.queue.get_mut(info_hash).ok_or(Error::TorrentDoesNotExist)?;
        let is_queue_empty = queue.is_empty();

        while let Some(block) = queue.pop_if(|v| *bitfield.safe_get(v.index)) {
            result.push(block);
        }

        let pieces_count = info.total_size.div_ceil(info.piece_length);

        if *req >= pieces_count
            && peer_ctx.block_infos_len.load(Ordering::Relaxed) == 0
            && is_queue_empty
            && result.is_empty()
        {
            let _ = self.enter_endgame(&peer_ctx.torrent_ctx).await;
        }

        Ok(result)
    }

    /// Request available metadata pieces.
    #[inline]
    pub fn request_metadata(
        &mut self,
        info_hash: &InfoHash,
        qnt: usize,
        metadata_size: usize,
    ) -> Result<Vec<MetadataPiece>, Error> {
        let metas = self.metadata_pieces.get_mut(info_hash);
        let metas = match metas {
            Some(metas) => metas,
            None => {
                let pieces = metadata_size.div_ceil(BLOCK_LEN);
                info!("meta pieces {pieces}");
                let pieces = (0..pieces).map(MetadataPiece).collect();
                self.metadata_pieces.insert(info_hash.clone(), pieces);
                self.metadata_pieces.get_mut(info_hash).unwrap()
            }
        };

        let v = metas.drain(0..qnt.min(metas.len())).collect();

        Ok(v)
    }

    async fn enter_endgame(
        &mut self,
        torrent_ctx: &Arc<TorrentCtx>,
    ) -> Result<(), Error> {
        let in_endgame = self
            .endgame
            .get_mut(&torrent_ctx.info_hash)
            .ok_or(Error::TorrentDoesNotExist)?;

        if *in_endgame {
            return Ok(());
        }

        *in_endgame = true;

        let _ = torrent_ctx.tx.send(TorrentMsg::Endgame).await;
        info!("disk endgame ʕノ•ᴥ•ʔノ ︵ ┻━┻");

        Ok(())
    }

    /// Open a file given a path.
    pub fn open_file(path: impl AsRef<Path>) -> Result<File, Error> {
        let path = path.as_ref().to_owned();
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|e| e.into())
    }

    pub fn open_file_dont_create(
        path: impl AsRef<Path>,
    ) -> Result<File, Error> {
        let path = path.as_ref().to_owned();
        OpenOptions::new()
            .read(true)
            .write(true)
            .create(false)
            .truncate(false)
            .open(&path)
            .map_err(|e| e.into())
    }

    /// Read the corresponding bytes of a block info from disk.
    pub fn read_block(
        &self,
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

        self.record_piece_access(info_hash.clone(), block_info.index);

        // get cached file handle
        let mmap = self.get_cached_read_mmap(path)?;

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
        torrent_ctx: Arc<TorrentCtx>,
        block: Block,
    ) -> Result<(), Error> {
        // Write the block's data to the correct position in the file
        // let piece_size = self.piece_size(info_hash, block.index)?;
        let len = block.block.len();
        let index = block.index;

        let cache = self
            .block_cache
            .get_mut(&torrent_ctx.info_hash)
            .ok_or(Error::TorrentDoesNotExist)?
            .entry(index)
            .or_default();

        // duplicate
        if cache.iter().any(|x| {
            x.index == index && x.begin == block.begin && x.block.len() == len
        }) {
            debug!("received duplicate block");
            return Ok(());
        }

        cache.push(block);

        // check if piece is fully downloaded
        if self
            .block_cache
            .get(&torrent_ctx.info_hash)
            .ok_or(Error::TorrentDoesNotExist)?
            .get(&index)
            .ok_or(Error::PieceInvalid)?
            .iter()
            .fold(0, |acc, v| acc + v.block.len())
            < self.piece_size(&torrent_ctx.info_hash, index)?
        {
            return Ok(());
        }

        // piece IS fully downloaded, verify and write

        // if the piece is corrupted, generate block infos
        if self.validate_piece(&torrent_ctx.info_hash, index).is_err() {
            let info = self
                .torrent_cache
                .get(&torrent_ctx.info_hash)
                .ok_or(Error::TorrentDoesNotExist)?;

            let info_blocks = Info::get_block_infos_of_piece(
                info.total_size,
                info.piece_length,
                index,
            );

            warn!("piece {index} is corrupted, generating more block infos",);

            self.queue
                .get_mut(&torrent_ctx.info_hash)
                .ok_or(Error::TorrentDoesNotExist)?
                .extend(info_blocks);

            return Ok(());
        }

        let downloaded_pieces_len = self
            .downloaded_pieces_len
            .get_mut(&torrent_ctx.info_hash)
            .ok_or(Error::TorrentDoesNotExist)?;

        *downloaded_pieces_len += 1;

        debug!("piece {index} is valid.");

        let _ = torrent_ctx.tx.send(TorrentMsg::DownloadedPiece(index)).await;

        if *downloaded_pieces_len == 4
            && *self
                .piece_strategy
                .get(&torrent_ctx.info_hash)
                .ok_or(Error::TorrentDoesNotExist)?
                == PieceStrategy::Random
        {
            debug!(
                "first piece downloaded, and piece order is random, switching
                  to rarest-first"
            );
            // self.set_piece_strategy(info_hash,
            // PieceStrategy::Rarest).await?;
        }

        // write the piece to disk
        self.write_piece(&torrent_ctx.info_hash, index)?;

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

    /// Remove the bytes of a piece from the cache.
    #[inline]
    fn get_piece_buffer(
        &mut self,
        info_hash: &InfoHash,
        piece_index: usize,
    ) -> Result<Vec<u8>, Error> {
        let blocks = self
            .block_cache
            .get_mut(info_hash)
            .and_then(|c| c.remove(&piece_index))
            .ok_or(Error::PieceInvalid)?;

        let total_length = blocks.iter().map(|b| b.block.len()).sum();

        // combine blocks into single contiguous buffer
        let mut piece_buffer = Vec::with_capacity(total_length);

        for block in blocks {
            piece_buffer.append(&mut block.block.into());
        }

        Ok(piece_buffer)
    }

    /// Write all cached blocks of `piece` to disk.
    /// It will free the blocks in the cache.
    #[inline]
    pub fn write_piece(
        &mut self,
        info_hash: &InfoHash,
        piece: usize,
    ) -> Result<(), Error> {
        let piece_buffer =
            self.get_piece_buffer(info_hash, piece)?.into_boxed_slice();
        self.record_piece_access(info_hash.clone(), piece);

        // a piece can be overlapped in one or many files,
        // do one write per file.
        let write_ops =
            self.calculate_write_ops(info_hash, piece, &piece_buffer)?;

        for (path, start, data_range) in write_ops {
            let mmap_arc = self.get_cached_write_mmap(&path)?;
            let piece_buffer = piece_buffer.clone();
            tokio::task::spawn_blocking(move || {
                let mut mmap =
                    mmap_arc.lock().expect("mmap poisoned in `write_piece`");
                let len = data_range.len();
                let end = start + len;
                mmap[start..end].copy_from_slice(&piece_buffer[data_range]);
                Ok::<(), Error>(())
            });
        }

        Ok(())
    }

    /// Sync all dirty pages of all files of the given torrent.
    #[inline]
    async fn sync_torrent(&self, hash: &InfoHash) -> Result<(), Error> {
        let cache = self
            .torrent_cache
            .get(hash)
            .ok_or(Error::TorrentDoesNotExist)?
            .clone();

        for f in &cache.file_metadata {
            let mmap = self.get_cached_write_mmap(&f.path)?;
            let mmap =
                mmap.lock().expect("mmap mut poisoned in `sync_torrent`");
            if let Err(e) = mmap.flush_async() {
                error!(
                    "Failed to flush file: {:?} with error: {:?}",
                    f.path, e
                );
            };
        }

        Ok(())
    }

    /// Get the correct piece size, the last piece of a torrent
    /// might be smaller than the other pieces.
    #[inline]
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
    #[inline]
    pub fn base_path(&self, info_hash: &InfoHash) -> PathBuf {
        let info = self.torrent_info.get(info_hash).unwrap();
        let mut base = self.config.download_dir.clone();
        base.push(&info.name);
        base
    }

    #[inline]
    fn get_cached_read_mmap(
        &self,
        path: &Arc<Path>,
    ) -> Result<Arc<Mmap>, Error> {
        let mmap = self.read_mmap_cache.get_or_insert_with::<_, Error>(
            path,
            || {
                // cache miss
                let file = Self::open_file(path)?;
                let mmap = unsafe { Mmap::map(&file)? };
                Ok(mmap.into())
            },
        )?;
        Ok(mmap)
    }

    #[inline]
    fn get_cached_write_mmap(
        &self,
        path: &Arc<Path>,
    ) -> Result<Arc<Mutex<MmapMut>>, Error> {
        let mmap = self.write_mmap_cache.get_or_insert_with::<_, Error>(
            path,
            || {
                // cache miss
                let file = Self::open_file(path)?;
                let mmap = unsafe { MmapMut::map_mut(&file) }?;
                let mmap = Arc::new(Mutex::new(mmap));
                Ok(mmap)
            },
        )?;

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

                if let Ok(file) = Self::open_file(&meta.path) {
                    file.set_len(meta.length as u64)?;
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

    /// Add a new torrent to Disk that came from the frontend.
    #[inline]
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
        let is_complete = self.is_torrent_complete(&info.info_hash);
        let is_incomplete = self.is_torrent_incomplete(&info.info_hash);

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

        self.compute_torrent_state(&info_hash, &downloaded_pieces).await?;

        Ok(())
    }

    /// Move the torrent metainfo to the torrents/complete folder
    async fn move_metainfo_to_dir(
        &self,
        info: &InfoHash,
        from_dir: MetadataDir,
        to_dir: MetadataDir,
    ) -> Result<(), Error> {
        let mut from_path = self.get_path_of(from_dir);

        from_path.push(info.to_string());
        from_path.set_extension("torrent");

        let mut to_path = self.get_path_of(to_dir);

        to_path.push(info.to_string());
        to_path.set_extension("torrent");

        let _ = tokio::fs::rename(from_path, to_path).await;

        Ok(())
    }

    fn is_torrent_complete(&self, hash: &InfoHash) -> bool {
        let mut path = self.complete_torrents_path();
        path.push(hash.to_string());
        path.set_extension("torrent");
        path.exists()
    }

    fn is_torrent_incomplete(&self, hash: &InfoHash) -> bool {
        let mut path = self.incomplete_torrents_path();
        path.push(hash.to_string());
        path.set_extension("torrent");
        path.exists()
    }

    #[inline]
    fn get_path_of(&self, dir: MetadataDir) -> PathBuf {
        match dir {
            MetadataDir::Queue => self.queue_torrents_path(),
            MetadataDir::Incomplete => self.incomplete_torrents_path(),
            MetadataDir::Complete => self.complete_torrents_path(),
        }
    }

    /// Read all .torrent files from (in)complete folder, and add them to the
    /// client.
    pub async fn read_metainfos_and_add(
        &mut self,
        metadata_dir: MetadataDir,
    ) -> Result<(), Error> {
        // ~/.config/vincenzo/torrents/incomplete
        let path = self.get_path_of(metadata_dir);
        let mut entries = tokio::fs::read_dir(path).await?;

        while let Some(entry) = entries.next_entry().await? {
            // ~/.config/vincenzo/torrents/incomplete/file.torrent
            let file_name = entry.path();

            let Ok(bytes) = tokio::fs::read(&file_name).await else { continue };
            let Ok(metainfo) = MetaInfo::from_bencode(&bytes) else { continue };

            if metadata_dir == MetadataDir::Queue {
                let mut new_name = file_name.clone();
                new_name.set_file_name(metainfo.info.info_hash.to_string());
                new_name.set_extension("torrent");
                tokio::fs::rename(file_name, new_name).await?;
            }

            // skip duplicate
            if self.torrent_ctxs.contains_key(&metainfo.info.info_hash) {
                continue;
            }

            if metadata_dir == MetadataDir::Queue {
                self.move_metainfo_to_dir(
                    &metainfo.info.info_hash,
                    MetadataDir::Queue,
                    MetadataDir::Incomplete,
                )
                .await?;
            }
            self.new_torrent_metainfo(metainfo, metadata_dir).await?;
        }
        Ok(())
    }

    #[inline]
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
    #[inline]
    #[tracing::instrument(skip(self, info_hash))]
    pub fn validate_piece(
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

        let blocks = self
            .block_cache
            .get_mut(info_hash)
            .ok_or(Error::TorrentDoesNotExist)?
            .get_mut(&index)
            .ok_or(Error::TorrentDoesNotExist)?;

        blocks.sort();

        let mut hasher = Sha1::new();
        for block in blocks {
            hasher.update(&block.block);
        }

        let hash = &hasher.finalize()[..];

        if *hash_from_info.as_slice() != *hash {
            return Err(Error::PieceInvalid);
        }

        Ok(())
    }

    /// Get the full path of a metainfo.
    #[inline]
    fn get_metainfo_path(
        &self,
        info_hash: &InfoHash,
    ) -> Result<PathBuf, Error> {
        if self.is_torrent_complete(info_hash) {
            let mut path = self.complete_torrents_path();
            path.push(info_hash.to_string());
            path.set_extension("torrent");
            return Ok(path);
        } else if self.is_torrent_incomplete(info_hash) {
            let mut path = self.incomplete_torrents_path();
            path.push(info_hash.to_string());
            path.set_extension("torrent");
            return Ok(path);
        }
        Err(Error::TorrentDoesNotExist)
    }

    pub fn set_piece_strategy(
        &mut self,
        info_hash: &InfoHash,
        s: PieceStrategy,
    ) -> Result<(), Error> {
        let current = self.piece_strategy.entry(info_hash.clone()).or_default();
        *current = s;
        let infos =
            self.queue.get_mut(info_hash).ok_or(Error::TorrentDoesNotExist)?;

        match s {
            PieceStrategy::Random => {
                infos.shuffle(&mut rand::rng());
            }
            PieceStrategy::Sequential => {
                infos.sort();
            }
            PieceStrategy::Rarest => {
                // self.rarest_first(info_hash).await?;
            }
        }

        Ok(())
    }

    /// Send a `DontNeed` advice to the file of the piece, in range.
    // a madvise per piece is probably fine.
    // piece sizes depend on the size of the torrent,
    // they are optimized to get close to 3000 pieces.
    // usually, small torrents < 1.5 KB have between 256kb - 512kb.
    // medium torrents > 10 GB around 4mb - 32mb.
    fn advise_piece_cold(&self, torrent: &Arc<TorrentCache>, piece: usize) {
        let page = page_size::get();
        let piece_len = torrent.piece_length(piece);
        let piece_start = piece * piece_len;
        let piece_end = piece_start + piece_len - 1;

        for file in &torrent.file_metadata {
            let file_start = file.start_offset;
            let file_end = file_start + file.length - 1;
            let file_length = file.length;
            let Some(mmap) = self.read_mmap_cache.get(&file.path) else {
                error!(
                    "advise_piece_cold: file path not found {:?}",
                    file.path
                );
                continue;
            };
            if !(piece_start <= file_end && piece_end >= file_start) {
                continue;
            };
            tokio::task::spawn_blocking(move || {
                let overlap_start = piece_start.max(file_start) - file_start;
                let overlap_end = piece_end.min(file_end) - file_start;
                let len = overlap_end - overlap_start + 1;

                // align to page
                let aligned_start = (overlap_start / page) * page;
                let aligned_end = (overlap_start + len).div_ceil(page) * page;
                let aligned_end = aligned_end.min(file_length);
                let aligned_len = aligned_end - aligned_start;

                unsafe {
                    if let Err(e) = mmap.unchecked_advise_range(
                        UncheckedAdvice::DontNeed,
                        aligned_start,
                        aligned_len,
                    ) {
                        error!("failed to advise DontNeed on mmap: {e}");
                    }
                }
            });
        }
    }

    /// For each cold piece of all torrents, send DontNeed.
    fn evict_cold_pieces(&self) -> Result<(), Error> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        let tracker = self
            .piece_tracker
            .read()
            .expect("lock poisoned on `evict_cold_pieces`");

        for (info_hash, tracker) in tracker.iter() {
            let torrent = self
                .torrent_cache
                .get(info_hash)
                .ok_or(Error::TorrentDoesNotExist)?;

            for (piece_idx, &last) in tracker.last_accessed.iter().enumerate() {
                if last != 0 && now.saturating_sub(last) > COLD_PIECE_THRESHOLD
                {
                    self.advise_piece_cold(torrent, piece_idx)
                }
            }
        }
        Ok(())
    }

    #[inline]
    fn record_piece_access(&self, info_hash: InfoHash, piece: usize) {
        let piece_tracker = self.piece_tracker.clone();
        tokio::task::spawn_blocking(move || {
            if let Some(tracker) = piece_tracker
                .write()
                .expect("lock poisoned `record_piece_access`")
                .get_mut(&info_hash)
            {
                tracker.record(piece);
            }
        });
    }
}
