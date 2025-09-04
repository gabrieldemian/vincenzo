//! Disk is responsible for file I/O of all torrents.

use bytes::Bytes;
use memmap2::{Mmap, MmapMut};
use rayon::iter::ParallelIterator;
use sha1::Digest;

use std::{
    collections::BTreeMap,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use bendy::{decoding::FromBencode, encoding::ToBencode};
use hashbrown::{HashMap, HashSet};
use lru::LruCache;
use rand::seq::SliceRandom;
use rayon::iter::IntoParallelIterator;
use sha1::Sha1;
use std::num::NonZeroUsize;
use tokio::{
    fs::{File, OpenOptions},
    io::{AsyncSeekExt, AsyncWriteExt},
    select,
    sync::{
        Mutex,
        mpsc::{self, Receiver},
        oneshot::{self, Sender},
    },
    time::interval,
};
use tracing::{debug, info, trace, warn};

use crate::{
    bitfield::{Bitfield, VczBitfield},
    config::CONFIG,
    daemon::DaemonMsg,
    error::Error,
    extensions::{
        BLOCK_LEN, MetadataPiece,
        core::{Block, BlockInfo},
    },
    metainfo::{Info, MetaInfo},
    peer::{PeerCtx, PeerId},
    torrent::{InfoHash, TorrentCtx, TorrentMsg},
};

// 64KB zero buffer
static ZERO_BUF: [u8; 65536] = [0; 65536];

#[derive(Debug)]
pub enum DiskMsg {
    /// After the client downloaded the Info from peers, this message will be
    /// sent, to create the skeleton of the torrent on disk (empty files
    /// and folders), and to add the torrent ctx.
    NewTorrent(Arc<TorrentCtx>, Info),

    MetadataSize(InfoHash, usize),

    /// The Peer does not have an ID until the handshake, when that happens,
    /// this message will be sent immediately to add the peer context.
    NewPeer(Arc<PeerCtx>),

    DeletePeer(SocketAddr),

    DeleteTorrent(InfoHash),

    Endgame(InfoHash),

    ReadBlock {
        info_hash: InfoHash,
        block_info: BlockInfo,
        recipient: Sender<Block>,
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
    // pub path: PathBuf,
    pub path: Arc<Path>,
    pub start_offset: u64,
    pub length: u64,
}

// Cache entry with precomputed metadata
#[derive(Debug, Clone)]
pub struct TorrentCache {
    pub file_metadata: Vec<FileMetadata>,
    pub piece_length: u64,
    pub total_size: u64,
    pub is_single_file_torrent: bool,
}

/// When a peer is Choked, or receives an error and must close the
/// connection, the outgoing/pending blocks of this peer must be
/// appended back to the list of available block_infos.
pub enum ReturnToDisk {
    Block(InfoHash, BTreeMap<usize, Vec<BlockInfo>>),
    Metadata(InfoHash, Vec<MetadataPiece>),
}

/// The Disk struct responsabilities:
/// - Open and create files, create directories
/// - Read/Write blocks to files
/// - Store block infos of all torrents
/// - Validate hash of pieces
pub struct Disk {
    pub tx: mpsc::Sender<DiskMsg>,
    daemon_tx: mpsc::Sender<DaemonMsg>,
    rx: Receiver<DiskMsg>,

    // pub(crate) free_tx: mpsc::UnboundedSender<ReturnToDisk>,
    free_rx: mpsc::UnboundedReceiver<ReturnToDisk>,

    pub(crate) torrent_ctxs: HashMap<InfoHash, Arc<TorrentCtx>>,

    pub(crate) peer_ctxs: Vec<Arc<PeerCtx>>,

    /// The sequence in which pieces will be downloaded,
    /// based on `PieceStrategy`.
    pub(crate) piece_order: HashMap<InfoHash, Vec<usize>>,

    /// Pieces that were requested and will be used to skip `piece_order`.
    pub(crate) pieces_requested: HashMap<InfoHash, Bitfield>,

    pub(crate) piece_strategy: HashMap<InfoHash, PieceStrategy>,

    /// How many pieces were downloaded.
    pub(crate) downloaded_pieces: HashMap<InfoHash, u64>,

    pub(crate) endgame: HashMap<InfoHash, bool>,

    /// A cache of blocks, where the key is a piece.
    block_cache: HashMap<InfoHash, BTreeMap<usize, Vec<Block>>>,

    /// A clone of Info to avoid locking.
    torrent_info: HashMap<InfoHash, Info>,

    /// The block infos of each piece of a torrent.
    block_infos: HashMap<InfoHash, BTreeMap<usize, Vec<BlockInfo>>>,

    metadata_pieces: HashMap<InfoHash, Vec<MetadataPiece>>,

    /// A cache of torrent files with pre-computed lengths.
    torrent_cache: HashMap<InfoHash, TorrentCache>,

    /// A LRU cache of file handles to avoid doing a sys call each time the
    /// disk needs to read or write to a file.
    read_mmap_cache: LruCache<PathBuf, Arc<Mmap>>,
    write_mmap_cache: LruCache<PathBuf, Arc<Mutex<MmapMut>>>,

    /// Files that need to be flushed, the bitfield is relative to
    /// `torrent_cache` files.
    dirty_files: HashMap<InfoHash, Bitfield>,
}

/// Cache capacity of files.
static FILE_CACHE_CAPACITY: usize = 512;

impl Disk {
    pub async fn new(
        daemon_tx: mpsc::Sender<DaemonMsg>,
        tx: mpsc::Sender<DiskMsg>,
        rx: mpsc::Receiver<DiskMsg>,
        free_rx: mpsc::UnboundedReceiver<ReturnToDisk>,
    ) -> Result<Self, Error> {
        let s = Self {
            dirty_files: HashMap::new(),
            daemon_tx,
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
            pieces_requested: HashMap::new(),
            block_cache: HashMap::new(),
            peer_ctxs: Vec::new(),
            torrent_ctxs: HashMap::new(),
            downloaded_pieces: HashMap::new(),
            endgame: HashMap::new(),
            piece_strategy: HashMap::default(),
            block_infos: HashMap::default(),
            torrent_info: HashMap::default(),
            piece_order: HashMap::default(),
        };

        // ensure the necessary folders are created.
        tokio::fs::create_dir_all(s.incomplete_torrents_path()).await?;
        tokio::fs::create_dir_all(s.complete_torrents_path()).await?;

        Ok(s)
    }

    #[tracing::instrument(name = "disk", skip_all)]
    pub async fn run(&mut self) -> Result<(), Error> {
        let mut flush_interval = interval(Duration::from_millis(100));
        let mut dirty_count_history = Vec::with_capacity(10);

        //
        // only for debugging
        //
        // let mut disk_interval = interval(Duration::from_secs(3));

        'outer: loop {
            select! {
                _ = flush_interval.tick() => {
                    let total_dirty: usize = self.dirty_files.values()
                        .map(|bits| bits.count_ones())
                        .sum();

                    dirty_count_history.push(total_dirty);
                    if dirty_count_history.len() > 10 {
                        dirty_count_history.remove(0);
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
                        // when idle
                        interval(Duration::from_millis(1000))
                    };

                    self.flush_dirty_files().await;
                    self.dirty_files.retain(|_, bits| bits.any());
                }
                // _ = disk_interval.tick() => {
                //     for (k, v) in self.block_infos.iter() {
                //         let pr = self.pieces_requested.get(k).unwrap();
                //         info!(
                //             "p: {} b: {} pr: {}",
                //             v.len(),
                //             v.values().fold(0, |acc, v| acc + v.len()),
                //             pr.count_ones(),
                //         );
                //     }
                // }
                Some(return_to_disk) = self.free_rx.recv() => {
                    match return_to_disk {
                        ReturnToDisk::Block(info_hash, blocks) => {
                            let endgame = self
                                .endgame
                                .get(&info_hash)
                                .ok_or(Error::TorrentDoesNotExist)?;

                            if *endgame {
                                let torrent_ctx = self
                                    .torrent_ctxs
                                    .get_mut(&info_hash)
                                    .ok_or(Error::TorrentDoesNotExist)?;
                                torrent_ctx.tx.send(TorrentMsg::Endgame(blocks)).await?;
                            } else {
                                for k in blocks.keys() {
                                    self.pieces_requested
                                        .get_mut(&info_hash)
                                        .ok_or(Error::TorrentDoesNotExist)?
                                        .set(*k, false);
                                }
                                self.block_infos
                                    .get_mut(&info_hash)
                                    .ok_or(Error::TorrentDoesNotExist)?
                                    .extend(blocks);
                            }
                        }
                        ReturnToDisk::Metadata(info_hash, pieces) => {
                            let meta_pieces = self
                                .metadata_pieces
                                .get_mut(&info_hash)
                                .ok_or(Error::TorrentDoesNotExist)?;
                            meta_pieces.extend(pieces);
                        }
                    };
                }
                Some(msg) = self.rx.recv() => {
                    match msg {
                        DiskMsg::DeleteTorrent(info_hash) => {
                            self.torrent_cache.remove_entry(&info_hash);
                            self.block_cache.remove_entry(&info_hash);
                            self.torrent_ctxs.remove_entry(&info_hash);
                            self.downloaded_pieces.remove_entry(&info_hash);
                            self.endgame.remove_entry(&info_hash);
                            self.piece_strategy.remove_entry(&info_hash);
                            self.block_infos.remove_entry(&info_hash);
                            self.metadata_pieces.remove_entry(&info_hash);
                            self.piece_order.remove_entry(&info_hash);
                            self.pieces_requested.remove_entry(&info_hash);
                            self.torrent_info.remove_entry(&info_hash);
                            self.dirty_files.remove_entry(&info_hash);
                            self.peer_ctxs.retain(|v| v.info_hash != info_hash);
                        }
                        DiskMsg::MetadataSize(info_hash, size) => {
                            if self.metadata_pieces.contains_key(
                                &info_hash,
                            ) { continue };

                            let pieces = size.div_ceil(BLOCK_LEN as usize);
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
                            trace!("delete_peer {addr:?}");
                            self.delete_peer(addr);
                        }
                        DiskMsg::NewTorrent(torrent, info) => {
                            let _ = self.new_torrent(torrent, info).await;
                        }
                        DiskMsg::ReadBlock { block_info, recipient, info_hash } => {
                            trace!("read_block");

                            let block =
                                self.read_block(&info_hash, &block_info).await?;

                            let _ = recipient.send(block);
                        }
                        DiskMsg::WriteBlock { block, info_hash } => {
                            self.write_block(&info_hash, block).await?;
                        }
                        DiskMsg::RequestBlocks { qnt, recipient, peer_id } => {
                            let infos = self
                                .request_blocks(&peer_id, qnt)
                                .await
                                .unwrap_or_default();

                            trace!("disk sending {} block infos", infos.len());

                            let _ = recipient.send(infos);
                        }
                        DiskMsg::RequestMetadata { qnt, recipient, info_hash } => {
                            let pieces = self
                                .request_metadata(&info_hash, qnt)
                                .unwrap_or_default();

                            let _ = recipient.send(pieces);
                        }
                        DiskMsg::ValidatePiece { info_hash, recipient, piece } => {
                            trace!("validate_piece");
                            let r = self.validate_piece(&info_hash, piece).await;
                            let _ = recipient.send(r);
                        }
                        DiskMsg::NewPeer(peer) => {
                            trace!("new_peer");
                            self.new_peer(peer);
                        }
                        DiskMsg::Quit => {
                            debug!("Quit");
                            break 'outer Ok(());
                        }
                    }
                }
            }
        }
    }

    async fn preopen_files<'a>(
        &self,
        file_metadata: &'a [FileMetadata],
    ) -> Result<Vec<(&'a FileMetadata, Arc<Mmap>)>, Error> {
        let mut mmaps = Vec::with_capacity(file_metadata.len());

        for file_meta in file_metadata {
            let file = File::open(&file_meta.path).await?;
            let mmap = unsafe { Mmap::map(&file)? };

            mmaps.push((file_meta, Arc::new(mmap)));
        }

        Ok(mmaps)
    }

    async fn handle_new_torrent_block_state(
        &mut self,
        info_hash: &InfoHash,
    ) -> Result<(), Error> {
        let torrent_cache = self
            .torrent_cache
            .get(info_hash)
            .ok_or(Error::TorrentDoesNotExist)?
            .clone();

        let info = self
            .torrent_info
            .get(info_hash)
            .ok_or(Error::TorrentDoesNotExist)?;

        let expected_hashes: Vec<[u8; 20]> = info
            .pieces
            .chunks_exact(20)
            .map(|chunk| chunk.try_into().unwrap())
            .collect();

        let total_pieces = info.pieces() as usize;
        let piece_length = info.piece_length as u64;
        let total_size = torrent_cache.total_size;

        let file_handles =
            self.preopen_files(&torrent_cache.file_metadata).await?;

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

        let mut downloaded_pieces = Bitfield::from_piece(total_pieces);
        let mut pieces_requested = Bitfield::from_piece(total_pieces);
        let block_infos =
            self.block_infos.entry(info_hash.clone()).or_default();

        for (piece_index, result) in piece_results.into_iter().enumerate() {
            if result {
                downloaded_pieces.set(piece_index, true);
                pieces_requested.set(piece_index, true);
                block_infos.insert(
                    piece_index,
                    info.get_block_infos_of_piece_self(piece_index),
                );
            }
        }

        // update state
        let downloaded_count = downloaded_pieces.count_ones() as u64;
        self.downloaded_pieces.insert(info_hash.clone(), downloaded_count);
        self.pieces_requested.insert(info_hash.clone(), pieces_requested);

        // generate block infos
        let block_infos = info.get_block_infos()?;
        self.block_infos.insert(info_hash.clone(), block_infos);

        info!(
            "verified {} downloaded pieces out of {} total pieces",
            downloaded_count, total_pieces
        );

        for (f, mmap) in file_handles {
            self.read_mmap_cache.put(f.path.to_path_buf(), mmap);
        }

        Ok(())
    }

    /// Initialize data for an incomplete torrent from a metainfo file. Data
    /// such as: how many block infos are still missing to be downloaded,
    /// downloaded pieces, etc.
    /// Initialize necessary data for a magnet torrent.
    /// Called after torrent has the info downloaded.
    async fn new_torrent(
        &mut self,
        torrent_ctx: Arc<TorrentCtx>,
        info: Info,
    ) -> Result<(), Error> {
        let info_hash = &torrent_ctx.info_hash;
        let total_size = info.get_size();
        let piece_length = info.piece_length as u64;

        self.write_incomplete_torrent_metainfo(&info).await?;
        self.torrent_ctxs.insert(info_hash.clone(), torrent_ctx.clone());
        self.torrent_info.insert(info_hash.clone(), info.clone());

        let mut file_metadata = Vec::new();
        let mut current_offset = 0;
        let base = self.base_path(info_hash);

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

        self.dirty_files.insert(
            info_hash.clone(),
            Bitfield::from_piece(file_metadata.len()),
        );

        self.torrent_cache.insert(
            info_hash.clone(),
            TorrentCache {
                file_metadata,
                piece_length,
                total_size,
                is_single_file_torrent: info.files.is_none(),
            },
        );

        // create folders, files, and preallocate them.
        self.preallocate_files(info_hash).await?;

        self.endgame.insert(info_hash.clone(), false);

        let piece_strategy =
            self.piece_strategy.entry(info_hash.clone()).or_default();

        let mut piece_order: Vec<usize> = (0..info.pieces() as usize).collect();

        if *piece_strategy == PieceStrategy::Random {
            piece_order.shuffle(&mut rand::rng());
        }

        self.piece_order.insert(info_hash.clone(), piece_order);

        self.handle_new_torrent_block_state(info_hash).await?;

        Ok(())
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
            .filter(|&v| v.info_hash == *info_hash)
            .cloned()
            .collect();

        debug!("calculating score of {:?} peers", peer_ctxs.len());

        if peer_ctxs.is_empty() {
            return Err(Error::NoPeers);
        }

        // pieces of the local peer
        let pieces = self
            .piece_order
            .get_mut(info_hash)
            .ok_or(Error::TorrentDoesNotExist)?;

        // vec of pieces scores/occurences, where index = piece.
        let mut score: Vec<u32> = vec![0u32; pieces.len()];

        // traverse pieces of the peers
        for ctx in peer_ctxs {
            let (otx, orx) = oneshot::channel();
            let _ = ctx
                .torrent_ctx
                .tx
                .send(TorrentMsg::GetPeerBitfield(ctx.id.clone(), otx))
                .await;

            let Ok(Some(pieces)) = orx.await else {
                continue;
            };

            for (i, item) in pieces.iter().enumerate() {
                // increment each occurence of a piece
                if *item && let Some(item) = score.get_mut(i) {
                    *item += 1;
                }
            }
        }

        while !score.is_empty() {
            // get the rarest, the piece with the least occurences
            let (rarest_idx, _) = score.iter().enumerate().min().unwrap();

            // get the last
            let (last_idx, _) = score.iter().enumerate().next_back().unwrap();

            pieces.swap(last_idx, rarest_idx);

            // remove the rarest from the score,
            // as it's already used.
            score.remove(rarest_idx);
        }

        let piece_strategy =
            self.piece_strategy.entry(info_hash.clone()).or_default();

        if *piece_strategy == PieceStrategy::Random {
            *piece_strategy = PieceStrategy::Rarest;
        }

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

    async fn flush_dirty_files(&mut self) {
        for (info_hash, dirty_bits) in &mut self.dirty_files {
            let mut to_set = Bitfield::from_piece(dirty_bits.len());

            let Some(cache) = self.torrent_cache.get(info_hash) else {
                continue;
            };
            for file_index in dirty_bits.iter_ones() {
                if let Some(file_meta) = cache.file_metadata.get(file_index)
                    && let Some(mmap) =
                        self.write_mmap_cache.get(&*file_meta.path)
                {
                    let mmap = mmap.lock().await;
                    if let Err(e) = mmap.flush_async() {
                        // keep it marked as dirty if flush failed
                    } else {
                        to_set.set(file_index, true);
                    }
                }
            }
            *dirty_bits &= !to_set;
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

        // todo: this can be precomputed. Keep a bitfield relative to the
        // piece_order. Only mutate it when receiving Have's from the peer.
        // And then it's possible to zip the bitfield with the piece order
        // and avoid the 2nd if statement.
        let (otx, orx) = oneshot::channel();
        peer_ctx
            .torrent_ctx
            .tx
            .send(TorrentMsg::GetMissingPieces(peer_id.clone(), otx))
            .await?;

        let pieces = orx.await?;
        let mut result = Vec::with_capacity(qnt);

        let piece_order = self
            .piece_order
            .get(&peer_ctx.info_hash)
            .ok_or(Error::TorrentDoesNotExist)?;

        let pieces_requested = self
            .pieces_requested
            .get_mut(&peer_ctx.info_hash)
            .ok_or(Error::TorrentDoesNotExist)?;

        if pieces_requested.count_ones() >= pieces_requested.len() {
            self.tx.send(DiskMsg::Endgame(peer_ctx.info_hash.clone())).await?;
        }

        for piece in piece_order.iter() {
            // if this piece was already requested, skip
            if *pieces_requested.safe_get(*piece) {
                continue;
            }

            // if this piece is not marked to be requested on `pieces`, skip
            if !unsafe { *pieces.get_unchecked(*piece) } {
                continue;
            };

            let Some(block_infos) = self
                .block_infos
                .get_mut(&peer_ctx.info_hash)
                .ok_or(Error::TorrentDoesNotExist)?
                .get_mut(piece)
            else {
                return Ok(vec![]);
            };

            let to_drain = qnt.saturating_sub(result.len());
            if to_drain == 0 {
                break;
            }

            result
                .extend(block_infos.drain(0..to_drain.min(block_infos.len())));

            if block_infos.is_empty() {
                pieces_requested.safe_set(*piece);

                self.block_infos
                    .get_mut(&peer_ctx.info_hash)
                    .ok_or(Error::TorrentDoesNotExist)?
                    .remove(piece);
            }

            if result.len() >= qnt {
                break;
            }
        }

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

    pub async fn read_block(
        &mut self,
        info_hash: &InfoHash,
        block_info: &BlockInfo,
    ) -> Result<Block, Error> {
        let cache = self
            .torrent_cache
            .get(info_hash)
            .ok_or(Error::TorrentDoesNotExist)?;

        // calculate absolute offset
        let absolute_offset = block_info.index as u64 * cache.piece_length
            + block_info.begin as u64;

        // find containing file
        let file_meta = cache
            .file_metadata
            .iter()
            .find(|m| {
                absolute_offset >= m.start_offset
                    && absolute_offset < m.start_offset + m.length
            })
            .ok_or(Error::TorrentDoesNotExist)?;

        let path = &file_meta.path.clone();

        // calculate file-relative offset
        let file_offset = absolute_offset - file_meta.start_offset;

        // get cached file handle
        let file = self.get_cached_read_mmap(path).await?;

        let start = file_offset as usize;
        let end = start + block_info.len as usize;

        // read data
        let buf = file[start..end].to_vec();

        Ok(Block {
            index: block_info.index as usize,
            begin: block_info.begin,
            block: buf,
        })
    }

    pub async fn read_block_zero_copy(
        &mut self,
        info_hash: &InfoHash,
        block_info: &BlockInfo,
    ) -> Result<Bytes, Error> {
        let cache = self
            .torrent_cache
            .get(info_hash)
            .ok_or(Error::TorrentDoesNotExist)?;

        // calculate absolute offset
        let absolute_offset = block_info.index as u64 * cache.piece_length
            + block_info.begin as u64;

        // find containing file
        let file_meta = cache
            .file_metadata
            .iter()
            .find(|m| {
                absolute_offset >= m.start_offset
                    && absolute_offset < m.start_offset + m.length
            })
            .ok_or(Error::TorrentDoesNotExist)?;

        // calculate file-relative offset
        let file_offset = absolute_offset - file_meta.start_offset;

        let start = file_offset as usize;
        let end = start + block_info.len as usize;

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
        let len = block.block.len();
        let index = block.index;

        let torrent_ctx = self
            .torrent_ctxs
            .get(info_hash)
            .ok_or(Error::TorrentDoesNotExist)?
            .clone();

        let cache = self
            .block_cache
            .get_mut(info_hash)
            .ok_or(Error::TorrentDoesNotExist)?
            .entry(index)
            .or_default();

        if cache.iter().any(|x| {
            x.index == index && x.begin == block.begin && x.block.len() == len
        }) {
            // duplicate
            return Ok(());
        }

        cache.push(block);

        // check if piece is fully downloaded
        if self
            .block_cache
            .get(info_hash)
            .ok_or(Error::TorrentDoesNotExist)?
            .get(&index)
            .ok_or(Error::PieceInvalid)?
            .iter()
            .fold(0, |acc, v| acc + v.block.len())
            < self.piece_size(info_hash, index)? as usize
        {
            return Ok(());
        }

        // validate the piece
        if self.validate_piece(info_hash, index).await.is_err() {
            let info = self
                .torrent_cache
                .get(info_hash)
                .ok_or(Error::TorrentDoesNotExist)?;

            let info_blocks = Info::get_block_infos_of_piece(
                info.total_size as usize,
                info.piece_length as usize,
                index,
            );

            warn!(
                "piece {index} is corrupted, generating more {} block infos",
                info_blocks.len()
            );

            self.block_infos
                .get_mut(info_hash)
                .ok_or(Error::TorrentDoesNotExist)?
                .entry(index)
                .or_default()
                .extend(info_blocks);

            return Ok(());
        }

        let downloaded_pieces_len = self
            .downloaded_pieces
            .get_mut(info_hash)
            .ok_or(Error::TorrentDoesNotExist)?;

        *downloaded_pieces_len += 1;

        debug!("piece {index} is valid.");

        let _ = torrent_ctx.tx.send(TorrentMsg::DownloadedPiece(index)).await;

        // write the piece to disk
        self.write_piece(info_hash, index).await?;

        if *self
            .piece_strategy
            .get(info_hash)
            .ok_or(Error::TorrentDoesNotExist)?
            == PieceStrategy::Random
        {
            debug!(
                "first piece downloaded, and piece order is random, switching \
                 to rarest-first"
            );
            self.rarest_first(info_hash).await?;
        }

        Ok(())
    }

    fn calculate_write_ops(
        &self,
        info_hash: &InfoHash,
        piece_index: usize,
        piece_buffer: &[u8],
    ) -> Result<Vec<(PathBuf, u64, std::ops::Range<usize>)>, Error> {
        let cache = self
            .torrent_cache
            .get(info_hash)
            .ok_or(Error::TorrentDoesNotExist)?;

        let piece_start = piece_index as u64 * cache.piece_length;
        let piece_end =
            (piece_start + cache.piece_length).min(cache.total_size);
        let piece_size = (piece_end - piece_start) as usize;

        // Validate piece buffer size
        if piece_buffer.len() < piece_size {
            return Err(Error::PieceInvalid);
        }

        let mut write_ops: Vec<(PathBuf, u64, std::ops::Range<usize>)> =
            Vec::new();

        for file_meta in &cache.file_metadata {
            let file_end = file_meta.start_offset + file_meta.length;
            let overlap_start = piece_start.max(file_meta.start_offset);
            let overlap_end = piece_end.min(file_end);

            if overlap_start >= overlap_end {
                continue;
            }

            let buffer_start = (overlap_start - piece_start) as usize;
            let buffer_end = (overlap_end - piece_start) as usize;
            let file_offset = overlap_start - file_meta.start_offset;

            write_ops.push((
                file_meta.path.to_path_buf(),
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
        piece_index: usize,
    ) -> Result<(), Error> {
        let piece_buffer = self.get_piece_buffer(info_hash, piece_index)?;

        // calculate write operations
        let write_ops =
            self.calculate_write_ops(info_hash, piece_index, &piece_buffer)?;

        // group writes by file
        let mut file_ops: HashMap<PathBuf, Vec<(u64, std::ops::Range<usize>)>> =
            HashMap::new();

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
                let start = file_offset as usize;
                let end = start + data_range.len();

                mmap[start..end].copy_from_slice(&piece_buffer[data_range]);
            }

            let _ = self.mark_file_dirty(
                info_hash,
                self.get_file_index(info_hash, &path).unwrap(),
            );
        }

        Ok(())
    }

    /// Remove the bytes of a piece from the cache.
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

        for mut block in blocks {
            piece_buffer.append(&mut block.block);
        }

        Ok(piece_buffer)
    }

    async fn write_piece_direct_io(
        &mut self,
        info_hash: &InfoHash,
        piece_index: usize,
    ) -> Result<(), Error> {
        let piece_buffer = self.get_piece_buffer(info_hash, piece_index)?;

        let write_ops =
            self.calculate_write_ops(info_hash, piece_index, &piece_buffer)?;

        // group writes by file
        let mut file_ops: HashMap<PathBuf, Vec<(u64, std::ops::Range<usize>)>> =
            HashMap::new();

        for (path, file_offset, data_range, ..) in write_ops {
            file_ops.entry(path).or_default().push((file_offset, data_range));
        }

        // write to each file using direct I/O
        for (path, ops) in file_ops {
            let mut file = Self::open_file(path).await?;

            // get file metadata to check if we're writing to the end
            let metadata = file.metadata().await?;
            let file_len = metadata.len();

            // sort operations by offset for sequential writes
            let mut ops = ops;
            ops.sort_by_key(|(offset, _)| *offset);

            let mut needs_sync = false;

            for (file_offset, data_range) in ops {
                let data = &piece_buffer[data_range];

                file.seek(std::io::SeekFrom::Start(file_offset)).await?;

                file.write_all(data).await?;

                // check if this write reaches the end of the file
                if file_offset + data.len() as u64 == file_len {
                    needs_sync = true;
                }
            }

            if needs_sync {
                file.sync_data().await?;
            }
        }

        Ok(())
    }

    /// Get the correct piece size, the last piece of a torrent
    /// might be smaller than the other pieces.
    fn piece_size(
        &self,
        info_hash: &InfoHash,
        piece_index: usize,
    ) -> Result<u32, Error> {
        let info = self
            .torrent_info
            .get(info_hash)
            .ok_or(Error::TorrentDoesNotExist)?;

        Ok(if piece_index == info.pieces() as usize - 1 {
            let remainder = info.get_size() % info.piece_length as u64;
            if remainder == 0 { info.piece_length } else { remainder as u32 }
        } else {
            info.piece_length
        })
    }
    /// Get the base path of a torrent directory.
    /// Which is always "download_dir/name_of_torrent".
    pub fn base_path(&self, info_hash: &InfoHash) -> PathBuf {
        let info = self.torrent_info.get(info_hash).unwrap();
        let mut base = CONFIG.download_dir.clone();
        base.push(&info.name);
        base
    }

    async fn get_cached_read_mmap(
        &mut self,
        path: &Path,
    ) -> Result<Arc<Mmap>, Error> {
        if let Some(mmap) = self.read_mmap_cache.get(path) {
            return Ok(mmap.clone());
        }

        // cache miss
        let file = Self::open_file(path).await?;
        let mmap = unsafe { Mmap::map(&file)? };
        let mmap_arc = Arc::new(mmap);

        self.read_mmap_cache.put(path.into(), mmap_arc.clone());
        Ok(mmap_arc)
    }

    async fn get_cached_write_mmap(
        &mut self,
        path: &Path,
    ) -> Result<Arc<Mutex<MmapMut>>, Error> {
        if let Some(mmap) = self.write_mmap_cache.get(path) {
            return Ok(mmap.clone());
        }

        // cache miss
        let file = Self::open_file(path).await?;
        let mmap = Arc::new(Mutex::new(unsafe { MmapMut::map_mut(&file)? }));

        self.write_mmap_cache.put(path.into(), mmap.clone());
        Ok(mmap)
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

        info!("endgame");

        *endgame = true;

        let torrent_ctx = self
            .torrent_ctxs
            .get(&info_hash)
            .ok_or(Error::TorrentDoesNotExist)?;

        let blocks = std::mem::take(
            self.block_infos
                .get_mut(&info_hash)
                .ok_or(Error::TorrentDoesNotExist)?,
        );

        torrent_ctx.tx.send(TorrentMsg::Endgame(blocks)).await?;

        Ok(())
    }

    async fn preallocate_files(
        &mut self,
        info_hash: &InfoHash,
    ) -> Result<(), Error> {
        if let Some(cache) = self.torrent_cache.get(info_hash) {
            for meta in &cache.file_metadata {
                if let Some(parent) = meta.path.parent() {
                    tokio::fs::create_dir_all(parent).await?;
                }

                if let Ok(file) = Self::open_file(&meta.path).await {
                    file.set_len(meta.length).await?;
                }
            }
        }
        Ok(())
    }

    fn complete_torrents_path(&self) -> PathBuf {
        let mut path = CONFIG.metadata_dir.clone();
        path.push("complete");
        path
    }

    fn incomplete_torrents_path(&self) -> PathBuf {
        let mut path = CONFIG.metadata_dir.clone();
        path.push("incomplete");
        path
    }

    /// Write an incomplete torrent info to disk
    async fn write_incomplete_torrent_metainfo(
        &self,
        info: &Info,
    ) -> Result<(), Error> {
        let mut path = self.incomplete_torrents_path();
        path.push(&info.name);
        let buff = info.to_bencode()?;
        let mut file = Self::open_file(path).await?;
        file.write_all(&buff).await?;

        Ok(())
    }

    /// Read all .torrent files from incomplete torrents, and add them to the
    /// client.
    async fn read_incomplete_torrents(&mut self) -> Result<(), Error> {
        // iterate over all .torrent files here
        let path = self.incomplete_torrents_path();

        let mut entries = tokio::fs::read_dir(path).await?;

        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();

            // only read .torrent files
            if path.extension().and_then(|s| s.to_str()) == Some("torrent") {
                continue;
            }

            let bytes = tokio::fs::read(&path).await?;
            let metainfo = MetaInfo::from_bencode(&bytes)?;

            // skip duplicate
            if self.torrent_ctxs.contains_key(&metainfo.info.info_hash) {
                continue;
            }

            let (otx, orx) = oneshot::channel();

            self.daemon_tx
                .send(DaemonMsg::NewTorrentMetaInfo(metainfo, otx))
                .await?;

            let (info, torrent_ctx) = orx.await?;

            self.new_torrent(torrent_ctx, info).await?;
        }

        Ok(())
    }

    fn verify_piece(
        piece_index: usize,
        mmaps: &[(&FileMetadata, Arc<Mmap>)],
        piece_length: u64,
        total_size: u64,
        expected_hash: [u8; 20],
    ) -> bool {
        let piece_start = (piece_index as u64) * piece_length;
        let piece_end = std::cmp::min(piece_start + piece_length, total_size);
        let piece_size = (piece_end - piece_start) as usize;

        let mut hasher = Sha1::new();
        let mut bytes_remaining = piece_size;

        for (file_meta, mmap) in mmaps {
            let file_start = file_meta.start_offset;
            let file_end = file_start + file_meta.length;

            // check if this file overlaps with the piece
            if piece_start < file_end && piece_end > file_start {
                let read_start = piece_start.max(file_start);
                let read_end = piece_end.min(file_end);
                let read_length = (read_end - read_start) as usize;

                // calculate file offset
                let file_offset = (read_start - file_start) as usize;

                // handle out-of-bounds access
                if file_offset >= mmap.len() {
                    // entire segment is beyond the file - hash zeros
                    let mut remaining_zeros = read_length;
                    while remaining_zeros > 0 {
                        let zero_chunk = remaining_zeros.min(ZERO_BUF.len());
                        hasher.update(&ZERO_BUF[..zero_chunk]);
                        remaining_zeros -= zero_chunk;
                    }
                    bytes_remaining -= read_length;
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

                // pad with zeros if needed
                if available_bytes < read_length {
                    let zero_bytes = read_length - available_bytes;
                    let mut remaining_zeros = zero_bytes;
                    while remaining_zeros > 0 {
                        let zero_chunk = remaining_zeros.min(ZERO_BUF.len());
                        hasher.update(&ZERO_BUF[..zero_chunk]);
                        remaining_zeros -= zero_chunk;
                    }
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
}

#[cfg(test)]
mod tests {
    use super::*;

    use futures::StreamExt;
    use rand::{Rng, distr::Alphanumeric};
    use std::{
        net::{Ipv4Addr, SocketAddrV4},
        time::Duration,
    };
    use tokio::{
        sync::{Mutex, broadcast},
        time::{Instant, interval, interval_at},
    };
    use tokio_util::codec::Framed;

    use crate::{
        bitfield::Reserved,
        counter::Counter,
        daemon::Daemon,
        extensions::{CoreCodec, core::BLOCK_LEN},
        magnet::Magnet,
        metainfo::{self, Info},
        peer::{
            self, DEFAULT_REQUEST_QUEUE_LEN, Peer, PeerMsg, RequestManager,
            StateLog,
        },
        torrent::{Connected, FromMagnet, PeerBrMsg, Stats, Torrent},
        tracker::{TrackerCtx, TrackerMsg},
    };

    use tokio::{
        net::{TcpListener, TcpStream},
        spawn,
        sync::mpsc,
    };

    // test all features that Disk provides by simulating, from start to end, a
    // remote peer that has the torrent fully downloaded and a local peer
    // trying to download it.
    #[tokio::test]
    async fn disk_works() -> Result<(), Error> {
        // =======================
        // preparing torrent files
        // =======================
        let mut rng = rand::rng();

        let download_dir: String =
            (0..32).map(|_| rng.sample(Alphanumeric) as char).collect();

        let torrent_dir = "bla".to_owned();

        let original_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |panic| {
            let _ = std::fs::remove_dir_all("/tmp/{download_dir}");
            original_hook(panic);
        }));

        let magnet = format!(
            "magnet:?xt=urn:btih:ce6adfa1642b882c910f88994b60229daff4e568&\
             dn={torrent_dir}&tr=http%3A% \
             2F%2Fnyaa.tracker.wf%3A7777%2Fannounce&tr=udp%3A%2F%2Fopen.\
             stealth.si%3A80% \
             2Fannounce&tr=udp%3A%2F%2Ftracker.opentrackr.org%3A1337%\
             2Fannounce&tr=udp%3A% \
             2F%2Fexodus.desync.com%3A6969%2Fannounce&tr=udp%3A%2F%2Ftracker.\
             torrent.eu. org%3A451%2Fannounce"
        );

        let files = vec![
            metainfo::File {
                length: BLOCK_LEN as u64 * 2,
                path: vec!["foo.txt".to_owned()],
            },
            metainfo::File {
                length: BLOCK_LEN as u64 * 2,
                path: vec!["bar".to_owned(), "baz.txt".to_owned()],
            },
            metainfo::File {
                length: BLOCK_LEN as u64 * 2,
                path: vec![
                    "bar".to_owned(),
                    "buzz".to_owned(),
                    "bee.txt".to_owned(),
                ],
            },
        ];

        // =======================
        // spawning boilerplate
        // =======================
        //
        // we will simulate a remote peer that is already connected, unchoked,
        // interested, ready to receive msgs like RequestBlocks etc.

        let (disk_tx, disk_rx) = mpsc::channel::<DiskMsg>(512);
        let (disk_free_tx, disk_free_rx) =
            mpsc::unbounded_channel::<ReturnToDisk>();

        let mut daemon = Daemon::new(disk_tx.clone(), disk_free_tx.clone());
        let magnet = Magnet::new(&magnet).unwrap();

        let daemon_ctx = daemon.ctx.clone();
        let _daemon_tx = daemon_ctx.tx.clone();

        let mut disk =
            Disk::new(_daemon_tx, disk_tx.clone(), disk_rx, disk_free_rx)
                .await
                .unwrap();

        let disk_tx = disk.tx.clone();
        let free_tx = disk_free_tx;

        let (tracker_tx, _tracker_rx) = mpsc::channel::<TrackerMsg>(100);
        let (torrent_tx, torrent_rx) = mpsc::channel::<TorrentMsg>(100);

        let mut hasher = Sha1::new();
        hasher.update([7u8; (BLOCK_LEN) as usize]);
        let hash = hasher.finalize();

        let mut pieces = vec![];
        for _ in 0..6 {
            pieces.extend(hash);
        }

        let info = Info {
            source: None,
            cross_seed_entry: None,
            piece_length: BLOCK_LEN,
            pieces,
            name: torrent_dir.clone(),
            file_length: None,
            files: Some(files.clone()),
            size: 0,
            info_hash: InfoHash::default(),
        };

        let pieces_len = info.pieces();

        let info_hash = magnet.parse_xt_infohash();

        let metadata_size = Some(1234);

        let (btx, _brx) = broadcast::channel::<PeerBrMsg>(500);

        let torrent_ctx = Arc::new(TorrentCtx {
            btx,
            free_tx: free_tx.clone(),
            disk_tx: disk_tx.clone(),
            tx: torrent_tx,
            info_hash: info_hash.clone(),
        });

        let (peer_tx, peer_rx) = mpsc::channel::<PeerMsg>(100);

        let peer_ctx = Arc::new(PeerCtx {
            torrent_ctx: torrent_ctx.clone(),
            remote_addr: SocketAddr::V4(SocketAddrV4::new(
                Ipv4Addr::new(127, 0, 0, 1),
                8080,
            )),
            local_addr: SocketAddr::V4(SocketAddrV4::new(
                Ipv4Addr::new(127, 0, 0, 1),
                8080,
            )),
            counter: Counter::default(),
            last_download_rate_update: Mutex::new(Instant::now()),
            info_hash: magnet.parse_xt_infohash(),
            tx: peer_tx.clone(),
            peer_interested: true.into(),
            id: PeerId::generate(),
            am_interested: true.into(),
            am_choking: false.into(),
            peer_choking: false.into(),
            direction: peer::Direction::Outbound,
        });

        disk.piece_strategy
            .insert(info_hash.clone(), PieceStrategy::Sequential);

        let listener = TcpListener::bind("0.0.0.0:0").await?;
        let local_addr = listener.local_addr()?;

        let pieces =
            bitvec::bitvec![u8, bitvec::prelude::Msb0; 1; pieces_len as usize];

        let peer_ctx_ = peer_ctx.clone();

        let stream = TcpStream::connect(local_addr).await?;

        // try to reconnect with errored peers
        let reconnect_interval = interval(Duration::from_secs(5));

        // send state to the frontend, if connected.
        let heartbeat_interval = interval(Duration::from_secs(1));

        let log_rates_interval = interval(Duration::from_secs(5));

        // unchoke the slowest interested peer.
        let optimistic_unchoke_interval = interval(Duration::from_secs(30));

        // unchoke algorithm:
        // - choose the best 3 interested uploaders and unchoke them.
        let unchoke_interval = interval_at(
            Instant::now() + Duration::from_secs(10),
            Duration::from_secs(10),
        );

        let announce_interval = interval_at(
            Instant::now() + Duration::from_secs(500),
            Duration::from_secs(500),
        );

        let mut torrent = Torrent {
            source: FromMagnet { magnet, info: Some(info.clone()) },
            state: Connected {
                reconnect_interval,
                heartbeat_interval,
                log_rates_interval,
                optimistic_unchoke_interval,
                unchoke_interval,
                announce_interval,
                peer_pieces: HashMap::from([(peer_ctx_.id.clone(), pieces)]),
                size: 0,
                counter: Counter::new(),
                unchoked_peers: Vec::new(),
                opt_unchoked_peer: None,
                connecting_peers: Vec::new(),
                error_peers: Vec::new(),
                bitfield: bitvec::bitvec![u8, bitvec::prelude::Msb0; 0; pieces_len as usize],
                stats: Stats { seeders: 1, leechers: 1, interval: 1000 },
                idle_peers: vec![],
                tracker_ctx: Arc::new(TrackerCtx {
                    tracker_addr: SocketAddr::V4(SocketAddrV4::new(
                        Ipv4Addr::new(0, 0, 0, 0),
                        0,
                    )),
                    tx: tracker_tx.clone(),
                }),
                metadata_size,
                connected_peers: vec![peer_ctx.clone()],
                info_pieces: BTreeMap::new(),
            },
            ctx: torrent_ctx.clone(),
            daemon_ctx,
            name: torrent_dir.clone(),
            rx: torrent_rx,
            status: crate::torrent::TorrentStatus::Downloading,
        };

        disk.new_peer(peer_ctx.clone());
        disk.new_torrent(torrent_ctx, info.clone()).await?;

        spawn(async move {
            let _ = disk.run().await;
            Ok::<(), Error>(())
        });

        spawn(async move {
            let _ = daemon.run().await;
            Ok::<(), Error>(())
        });

        spawn(async move {
            let _ = torrent.run().await;
            Ok::<(), Error>(())
        });

        spawn(async move {
            let (_socket, _) = listener.accept().await.unwrap();
            let socket = Framed::new(stream, CoreCodec);
            let (sink, stream) = socket.split();

            let mut peer = Peer::<peer::Connected> {
                state_log: StateLog::default(),
                state: peer::Connected {
                    free_tx,
                    is_paused: false,
                    ctx: peer_ctx_,
                    ext_states: peer::ExtStates::default(),
                    have_info: true,
                    in_endgame: false,
                    incoming_requests: Vec::new(),
                    req_man_meta: RequestManager::new(),
                    req_man_block: RequestManager::new(),
                    reserved: Reserved::default(),
                    rx: peer_rx,
                    seed_only: false,
                    sink,
                    stream,
                    target_request_queue_len: DEFAULT_REQUEST_QUEUE_LEN,
                },
            };

            let _ = peer.run().await;
        });

        let (otx, orx) = oneshot::channel();
        disk_tx
            .send(DiskMsg::RequestBlocks {
                peer_id: peer_ctx.id.clone(),
                recipient: otx,
                qnt: 3,
            })
            .await?;

        let blocks = orx.await?;

        assert_eq!(
            blocks,
            vec![
                BlockInfo { index: 0, begin: 0, len: BLOCK_LEN },
                BlockInfo { index: 1, begin: 0, len: BLOCK_LEN },
                BlockInfo { index: 2, begin: 0, len: BLOCK_LEN },
            ]
        );

        disk_tx
            .send(DiskMsg::WriteBlock {
                info_hash: info_hash.clone(),
                block: Block {
                    index: 1,
                    begin: 0,
                    block: vec![7u8; BLOCK_LEN as usize],
                },
            })
            .await?;

        let (otx, orx) = oneshot::channel();

        disk_tx
            .send(DiskMsg::ReadBlock {
                info_hash: info_hash.clone(),
                recipient: otx,
                block_info: BlockInfo { index: 1, begin: 0, len: BLOCK_LEN },
            })
            .await?;

        let block = orx.await?;

        assert_eq!(block.index, 1);
        assert_eq!(block.begin, 0);
        assert_eq!(block.block.len(), blocks[0].len as usize);

        // let _ = tokio::fs::remove_dir_all(format!("/tmp/{download_dir}")).
        // await;

        Ok(())
    }
}
