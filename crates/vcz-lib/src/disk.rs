//! Disk is responsible for file I/O info_hash, pieces all torrents.

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
    collections::HashMap,
    fs::File,
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
    AddTorrent(Arc<MetaInfo>),

    DeleteTorrent(InfoHash, bool),

    FinishedDownload(InfoHash),

    ReadBlock {
        ctx: Arc<TorrentCtx>,
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
#[derive(Debug, Clone, Default)]
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

pub struct TorrentData {
    /// bytes downloaded per piece, to know when to compute hashes
    downloaded_per_piece: Vec<usize>,
    /// To track when endgame should start
    requested_pieces_len: usize,
    endgame: bool,
    piece_tracker: Arc<RwLock<PieceTracker>>,
    torrent_info: Arc<Info>,
    metadata_pieces: Vec<MetadataPiece>,
    piece_strategy: PieceStrategy,
    queue: Vec<BlockInfo>,
    /// A cache of torrent files with pre-computed lengths.
    torrent_cache: Arc<TorrentCache>,
}

/// The Disk struct responsabilities:
/// - Open and create files, create directories
/// - Read/Write blocks to files
/// - Validate hash of pieces
/// - Answer messages
pub struct Disk {
    pub ctx: HashMap<InfoHash, Arc<TorrentCtx>>,
    pub config: Arc<ResolvedConfig>,
    pub tx: mpsc::Sender<DiskMsg>,
    pub(crate) daemon_ctx: Arc<DaemonCtx>,
    pub(crate) rx: Receiver<DiskMsg>,
    pub(crate) free_rx: mpsc::UnboundedReceiver<ReturnToDisk>,
    pub(crate) read_mmap_cache: Cache<Arc<Path>, Arc<Mmap>>,
    pub(crate) write_mmap_cache: Cache<Arc<Path>, Arc<Mutex<MmapMut>>>,
    pub torrents: HashMap<InfoHash, TorrentData>,
}

pub struct PieceTracker(Box<[u64]>);

impl PieceTracker {
    fn record(&mut self, piece: usize) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        self.0[piece] = now;
    }
}

macro_rules! torrent_data {
    ($self:expr, $info:expr, $field:ident) => {
        $self
            .torrents
            .get($info)
            .map(|v| &v.$field)
            .ok_or(Error::TorrentDoesNotExist)
    };
}

macro_rules! torrent_data_mut {
    ($self:expr, $info:expr, $field:ident) => {
        $self
            .torrents
            .get_mut($info)
            .map(|v| &mut v.$field)
            .ok_or(crate::Error::TorrentDoesNotExist)
    };
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
            torrents: HashMap::new(),
            ctx: HashMap::new(),
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
            std::fs::create_dir_all(self.incomplete_torrents_path())?;
            std::fs::create_dir_all(self.complete_torrents_path())?;
            std::fs::create_dir_all(self.queue_torrents_path())?;
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
                            let Ok(metainfo_path) = self.get_metainfo_path(&info_hash)
                            else {
                                continue;
                            };
                            let Ok(files_path) = torrent_data!(self, &info_hash, torrent_cache)
                            else {
                                continue;
                            };
                            let files_path = files_path.file_metadata[0].path.clone();

                            info!("metainfo {metainfo_path:?}");
                            info!("disk path {files_path:?}");

                            tokio::task::spawn_blocking(move || {
                                let _ = std::fs::remove_file(metainfo_path);
                                if also_from_disk {
                                    if files_path.is_file() {
                                        let _ = std::fs::remove_file(files_path);
                                    } else {
                                        let _ = std::fs::remove_dir_all(files_path);
                                    }
                                }
                            });

                            self.torrents.remove(&info_hash);
                        }
                        DiskMsg::FinishedDownload(info_hash) => {
                            let _ = self.sync_torrent(&info_hash).await;
                            let _ = self.move_metainfo_to_dir(
                                &info_hash,
                                MetadataDir::Incomplete,
                                MetadataDir::Complete
                            );
                            let Some(v) = self.torrents.get_mut(&info_hash) else {
                                continue;
                            };
                            v.queue.clear();
                            v.metadata_pieces.clear();
                            v.downloaded_per_piece.clear();
                        }
                        DiskMsg::AddTorrent(meta) => {
                            let _ = self.new_torrent(meta, MetadataDir::Queue).await;
                        }
                        DiskMsg::ReadBlock { block_info, recipient, ctx } => {
                            let r = self.read_block(&ctx.info_hash, &block_info);
                            if let Err(e) = &r {
                                self.handle_torrent_err(ctx, e).await;
                            }
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
                            let Ok(infos) = torrent_data_mut!(self, &info_hash, queue) else {
                                continue;
                            };
                            infos.extend(blocks);
                        }
                        ReturnToDisk::Metadata(info_hash, pieces) => {
                            let Ok(m) = torrent_data_mut!(self, &info_hash, metadata_pieces) else {
                                continue;
                            };
                            m.extend(pieces);
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
    /// [`DaemonMsg::AddTorrent`].
    #[tracing::instrument(skip_all, fields(name = %meta.info.name))]
    pub async fn new_torrent(
        &mut self,
        meta: Arc<MetaInfo>,
        metadata_dir: MetadataDir,
    ) -> Result<(), Error> {
        let info_hash = meta.info.info_hash.clone();
        let info = Arc::new(meta.info.clone());

        self.new_torrent_data(info.clone());
        self.compute_torrent_cache(&info)?;

        if metadata_dir == MetadataDir::Queue {
            // create folders, files, and preallocate them with zeroes.
            self.pre_alloc_files(&info_hash)?;
        }

        let mut is_err = false;
        let (downloaded_pieces, dp) =
            match self.compute_downloaded_pieces(&info) {
                Ok(v) => v,
                Err(Error::TorrentFilesMissing(downloaded_pieces, dp)) => {
                    is_err = true;
                    (downloaded_pieces, dp)
                }
                Err(e) => return Err(e),
            };

        *torrent_data_mut!(self, &info_hash, requested_pieces_len)? = dp;
        self.set_piece_strategy(&info_hash, PieceStrategy::default())?;

        let is_complete = dp >= info.pieces();
        let info_hash = info.info_hash.clone();

        let mut torrent = Torrent::new_metainfo(
            self.config.clone(),
            self.tx.clone(),
            self.daemon_ctx.clone(),
            meta,
            downloaded_pieces,
        );
        self.ctx.insert(info_hash.clone(), torrent.ctx.clone());

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
            )?;
        }

        self.daemon_ctx
            .tx
            .send(DaemonMsg::AddTorrent(Box::new(torrent)))
            .await?;

        Ok(())
    }

    #[inline]
    #[allow(clippy::type_complexity)]
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
    fn compute_downloaded_pieces(
        &mut self,
        info: &Info,
    ) -> Result<(Bitfield, usize), Error> {
        let torrent_cache =
            torrent_data!(self, &info.info_hash, torrent_cache)?;

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
        let mut downloaded_per_piece = vec![0_usize; total_pieces];

        for (piece, result) in piece_results.into_iter().enumerate() {
            downloaded_pieces.set(piece, result);
            if result {
                downloaded_per_piece[piece] = info.piece_length(piece);
            }
        }

        *torrent_data_mut!(self, &info.info_hash, downloaded_per_piece)? =
            downloaded_per_piece;

        let ones = downloaded_pieces.count_ones();
        tracing::debug!(
            "computed {} pieces, {} downloaded",
            downloaded_pieces.len(),
            ones,
        );

        if is_err {
            return Err(Error::TorrentFilesMissing(downloaded_pieces, ones));
        }
        Ok((downloaded_pieces, ones))
    }

    #[inline]
    fn compute_torrent_cache(&mut self, info: &Arc<Info>) -> Result<(), Error> {
        let mut file_metadata = Vec::new();
        let mut current_offset = 0;
        let base = self.base_path(&info.info_hash);
        let num_pieces = info.pieces();

        let mut lock = torrent_data!(self, &info.info_hash, piece_tracker)?
            .write()
            .expect("lock piece_tracker compute_torrent_cache");
        let last_accessed = vec![0; num_pieces].into_boxed_slice();
        *lock = PieceTracker(last_accessed);
        drop(lock);

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

        *torrent_data_mut!(self, &info.info_hash, torrent_cache)? =
            Arc::new(TorrentCache {
                file_metadata,
                piece_length: info.piece_length,
                total_size: info.get_torrent_size(),
                num_pieces,
            });

        Ok(())
    }

    /// Request available block infos following the order of PieceStrategy.
    #[inline]
    pub async fn request_blocks(
        &mut self,
        peer_ctx: Arc<PeerCtx>,
        pieces: Vec<usize>,
        mut bitfield: Box<Bitfield>,
    ) -> Result<Vec<BlockInfo>, Error> {
        let info_hash = &peer_ctx.torrent_ctx.info_hash;
        let total_size =
            torrent_data!(self, info_hash, torrent_cache)?.total_size;
        let piece_length =
            torrent_data!(self, info_hash, torrent_cache)?.piece_length;
        let req = torrent_data_mut!(self, info_hash, requested_pieces_len)?;
        let per_piece = piece_length.div_ceil(BLOCK_LEN);
        let pieces_count = total_size.div_ceil(piece_length);
        let mut result = Vec::with_capacity(per_piece * pieces.len());

        for p in pieces {
            result.extend(Info::get_block_infos_of_piece(
                total_size,
                piece_length,
                p,
            ));
            *req += 1;
        }

        let queue = torrent_data_mut!(self, info_hash, queue)?;
        let is_queue_empty = queue.is_empty();

        while let Some(block) = queue.pop_if(|v| *bitfield.safe_get(v.index)) {
            result.push(block);
        }

        let req = torrent_data!(self, info_hash, requested_pieces_len)?;
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
        let metas = torrent_data_mut!(self, info_hash, metadata_pieces)?;
        if metas.is_empty() {
            let pieces = metadata_size.div_ceil(BLOCK_LEN);
            *metas = (0..pieces).map(MetadataPiece).collect();
        }
        let v = metas.drain(0..qnt.min(metas.len())).collect();
        Ok(v)
    }

    #[inline]
    async fn enter_endgame(
        &mut self,
        torrent_ctx: &Arc<TorrentCtx>,
    ) -> Result<(), Error> {
        let in_endgame =
            torrent_data_mut!(self, &torrent_ctx.info_hash, endgame)?;

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

    /// Read the corresponding bytes of a block info from disk.
    #[inline]
    pub(crate) fn read_block(
        &self,
        info_hash: &InfoHash,
        block_info: &BlockInfo,
    ) -> Result<Bytes, Error> {
        let torrent_cache = torrent_data!(self, info_hash, torrent_cache)?;

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

        self.record_piece_access(info_hash.clone(), block_info.index)?;

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
    #[inline]
    pub(crate) async fn write_block(
        &mut self,
        torrent_ctx: Arc<TorrentCtx>,
        block: Block,
    ) -> Result<(), Error> {
        let len = block.block.len();
        let index = block.index;

        if !self.is_piece_downloaded(index, &torrent_ctx.info_hash)? {
            return Ok(());
        }

        // piece IS fully downloaded, verify and write

        // if the piece is corrupted, generate block infos
        if self.validate_piece(&torrent_ctx.info_hash, index).is_err() {
            let info =
                torrent_data!(self, &torrent_ctx.info_hash, torrent_cache)?;

            let info_blocks = Info::get_block_infos_of_piece(
                info.total_size,
                info.piece_length,
                index,
            );

            warn!("piece {index} is corrupted, generating more block infos",);

            torrent_data_mut!(self, &torrent_ctx.info_hash, queue)?
                .extend(info_blocks);

            return Ok(());
        }

        let downloaded_pieces_len = torrent_data_mut!(
            self,
            &torrent_ctx.info_hash,
            downloaded_per_piece
        )?;

        downloaded_pieces_len[index] += len;

        tracing::trace!("piece {index} is valid.");

        let _ = torrent_ctx.tx.send(TorrentMsg::DownloadedPiece(index)).await;

        let ops = self.calculate_write_ops_for_block(
            &torrent_ctx.info_hash,
            &BlockInfo { len, index, begin: block.begin },
        )?;

        for (path, file_offset, range) in ops {
            let mmap_arc = self.get_cached_write_mmap(&path)?;
            let b = block.block.clone();
            let mut mmap =
                mmap_arc.lock().expect("mmap poisoned in `write_block`");
            mmap[file_offset..file_offset + range.len()]
                .copy_from_slice(&b[range]);
        }

        Ok(())
    }

    #[allow(clippy::type_complexity)]
    #[inline]
    fn calculate_write_ops_for_block(
        &self,
        info_hash: &InfoHash,
        block_info: &BlockInfo,
    ) -> Result<Vec<(Arc<Path>, usize, std::ops::Range<usize>)>, Error> {
        let cache = torrent_data!(self, info_hash, torrent_cache)?;

        let block_start =
            block_info.index * cache.piece_length + block_info.begin;
        let block_end = block_start + block_info.len;

        // clamp to torrent size (should not exceed, but safety)
        let block_end = block_end.min(cache.total_size);

        let mut write_ops = Vec::with_capacity(cache.file_metadata.len());

        for file_meta in &cache.file_metadata {
            let file_end = file_meta.start_offset + file_meta.length;

            let overlap_start = block_start.max(file_meta.start_offset);
            let overlap_end = block_end.min(file_end);

            if overlap_start >= overlap_end {
                continue;
            }

            let buffer_start = overlap_start - block_start;
            let buffer_end = overlap_end - block_start;

            let file_offset = overlap_start - file_meta.start_offset;

            write_ops.push((
                file_meta.path.clone(),
                file_offset,
                buffer_start..buffer_end,
            ));
        }

        Ok(write_ops)
    }

    #[allow(clippy::type_complexity)]
    fn calculate_write_ops(
        &self,
        info_hash: &InfoHash,
        piece_index: usize,
        piece_buffer: &[u8],
    ) -> Result<Vec<(Arc<Path>, usize, std::ops::Range<usize>)>, Error> {
        let cache = torrent_data!(self, info_hash, torrent_cache)?;
        let piece_start = piece_index * cache.piece_length;
        let piece_end =
            (piece_start + cache.piece_length).min(cache.total_size);
        let piece_size = piece_end - piece_start;

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

    /// Sync all dirty pages of all files of the given torrent.
    #[inline]
    async fn sync_torrent(&self, hash: &InfoHash) -> Result<(), Error> {
        let cache = torrent_data!(self, hash, torrent_cache)?;

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

    /// Get the base path of a torrent directory.
    /// Which is always "download_dir/name_of_torrent".
    #[inline]
    pub(crate) fn base_path(&self, info_hash: &InfoHash) -> PathBuf {
        let info = torrent_data!(self, info_hash, torrent_info).unwrap();
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

    fn pre_alloc_files(&mut self, info_hash: &InfoHash) -> Result<(), Error> {
        if let Ok(cache) = torrent_data!(self, info_hash, torrent_cache) {
            for meta in &cache.file_metadata {
                if let Some(parent) = meta.path.parent() {
                    std::fs::create_dir_all(parent)?;
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

    /// Move the torrent metainfo to the torrents/complete folder
    fn move_metainfo_to_dir(
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

        let _ = std::fs::rename(from_path, to_path);

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
        let mut entries = std::fs::read_dir(path)?;

        while let Some(Ok(entry)) = entries.next() {
            // ~/.config/vincenzo/torrents/incomplete/file.torrent
            let file_name = entry.path();

            let Ok(bytes) = std::fs::read(&file_name) else { continue };
            let Ok(metainfo) = MetaInfo::from_bencode(&bytes) else { continue };

            if metadata_dir == MetadataDir::Queue {
                let mut new_name = file_name.clone();
                new_name.set_file_name(metainfo.info.info_hash.to_string());
                new_name.set_extension("torrent");
                std::fs::rename(file_name, new_name)?;
            }

            // skip duplicate
            if self.torrents.contains_key(&metainfo.info.info_hash) {
                continue;
            }

            if metadata_dir == MetadataDir::Queue {
                self.move_metainfo_to_dir(
                    &metainfo.info.info_hash,
                    MetadataDir::Queue,
                    MetadataDir::Incomplete,
                )?;
            }
            let meta = Arc::new(metainfo);
            self.new_torrent(meta, metadata_dir).await?;
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
    fn validate_piece(
        &mut self,
        info_hash: &InfoHash,
        piece: usize,
    ) -> Result<(), Error> {
        let b = piece * 20;
        let e = b + 20;
        let info = torrent_data!(self, info_hash, torrent_info)?;
        let torrent = torrent_data!(self, info_hash, torrent_cache)?;
        let hash_from_info = &info.pieces[b..e];

        let mut hasher = Sha1::new();

        let piece_start = piece * info.piece_length;
        let piece_len = info.piece_length(piece);
        let piece_end = piece_start + piece_len - 1;

        for file in &torrent.file_metadata {
            let file_start = file.start_offset;
            let file_end = file_start + file.length - 1;
            if piece_start <= file_end && piece_end >= file_start {
                let overlap_start = piece_start.max(file_start) - file_start;
                let overlap_end = piece_end.min(file_end) - file_start;
                let len = overlap_end - overlap_start + 1;
                let mmap = self.get_cached_read_mmap(&file.path)?;
                hasher.update(&mmap[overlap_start..][..len]);
            }
        }

        let hash = &hasher.finalize()[..];

        if *hash_from_info != *hash {
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

    fn set_piece_strategy(
        &mut self,
        info_hash: &InfoHash,
        s: PieceStrategy,
    ) -> Result<(), Error> {
        let current = torrent_data_mut!(self, info_hash, piece_strategy)?;
        *current = s;
        let infos = torrent_data_mut!(self, info_hash, queue)?;

        match s {
            PieceStrategy::Random => {
                infos.shuffle(&mut rand::rng());
            }
            PieceStrategy::Sequential => {
                infos.sort();
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
    #[inline]
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

        for (info_hash, tracker) in self.torrents.iter() {
            let tracker = tracker.piece_tracker.read().unwrap();
            let torrent = torrent_data!(self, info_hash, torrent_cache)?;

            for (piece_idx, &last) in tracker.0.iter().enumerate() {
                if last != 0 && now.saturating_sub(last) > COLD_PIECE_THRESHOLD
                {
                    self.advise_piece_cold(torrent, piece_idx)
                }
            }
        }
        Ok(())
    }

    #[inline]
    fn record_piece_access(
        &self,
        info_hash: InfoHash,
        piece: usize,
    ) -> Result<(), Error> {
        let piece_tracker =
            torrent_data!(self, &info_hash, piece_tracker)?.clone();
        tokio::task::spawn_blocking(move || {
            if let Ok(mut tracker) = piece_tracker.write() {
                tracker.record(piece);
            } else {
                error!("piece_tracker lock poisoned on `record_piece_access`")
            }
        });
        Ok(())
    }

    #[inline]
    fn new_torrent_data(&mut self, torrent_info: Arc<Info>) {
        self.torrents.insert(
            torrent_info.info_hash.clone(),
            TorrentData {
                requested_pieces_len: 0,
                downloaded_per_piece: Vec::new(),
                endgame: false,
                piece_tracker: Arc::new(RwLock::new(PieceTracker(
                    Box::new([]),
                ))),
                torrent_info,
                metadata_pieces: Vec::new(),
                piece_strategy: PieceStrategy::default(),
                queue: Vec::new(),
                torrent_cache: Arc::new(TorrentCache::default()),
            },
        );
    }

    #[inline]
    fn is_piece_downloaded(
        &self,
        piece: usize,
        info_hash: &InfoHash,
    ) -> Result<bool, Error> {
        let info = torrent_data!(self, info_hash, torrent_cache)?;
        let downloaded = torrent_data!(self, info_hash, downloaded_per_piece)?;
        Ok(downloaded[piece] >= info.piece_length(piece))
    }
}
