//! Disk is responsible for file I/O of all Torrents.
use std::{
    collections::BTreeMap,
    io::SeekFrom,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
};

use futures::future::join_all;
use hashbrown::HashMap;
use lru::LruCache;
use rand::seq::SliceRandom;
use std::num::NonZeroUsize;
use tokio::{
    fs::{File, OpenOptions},
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
    sync::{
        mpsc::{self, Receiver},
        oneshot::{self, Sender},
    },
};
use tracing::{debug, info, trace, warn};

use crate::{
    bitfield::Bitfield,
    error::Error,
    extensions::core::{Block, BlockInfo},
    metainfo::Info,
    peer::{PeerCtx, PeerId, PeerMsg},
    torrent::{InfoHash, TorrentCtx, TorrentMsg},
};

#[derive(Debug)]
pub enum DiskMsg {
    /// After the client downloaded the Info from peers, this message will be
    /// sent, to create the skeleton of the torrent on disk (empty files
    /// and folders), and to add the torrent ctx.
    NewTorrent(Arc<TorrentCtx>),

    /// The Peer does not have an ID until the handshake, when that happens,
    /// this message will be sent immediately to add the peer context.
    NewPeer(Arc<PeerCtx>),

    DeletePeer(SocketAddr),

    DeleteTorrent(InfoHash),

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
        info_hash: InfoHash,
        peer_id: PeerId,
        peer_pieces: Bitfield,
        recipient: Sender<Vec<BlockInfo>>,
        qnt: usize,
    },

    /// When a peer is Choked, or receives an error and must close the
    /// connection, the outgoing/pending blocks of this peer must be
    /// appended back to the list of available block_infos.
    ReturnBlockInfos(InfoHash, Vec<BlockInfo>),
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
    pub path: PathBuf,
    pub start_offset: u64,
    pub length: u64,
}

// Cache entry with precomputed metadata
#[derive(Debug, Clone)]
pub struct TorrentCache {
    pub file_metadata: Vec<FileMetadata>,
    pub piece_length: u64,
    pub total_size: u64,
}

/// The Disk struct responsabilities:
/// - Open and create files, create directories
/// - Read/Write blocks to files
/// - Store block infos of all torrents
/// - Validate hash of pieces
#[derive(Debug)]
pub struct Disk {
    pub tx: mpsc::Sender<DiskMsg>,
    pub torrent_ctxs: HashMap<InfoHash, Arc<TorrentCtx>>,
    pub peer_ctxs: Vec<Arc<PeerCtx>>,

    /// The sequence in which pieces will be downloaded,
    /// based on `PieceOrder`.
    pub piece_order: HashMap<InfoHash, Vec<u32>>,

    /// How many pieces were downloaded.
    pub downloaded_pieces_len: HashMap<InfoHash, u32>,

    pub piece_strategy: HashMap<InfoHash, PieceStrategy>,

    pub download_dir: String,

    /// A cache of blocks, where the key is a piece.
    cache: HashMap<InfoHash, BTreeMap<usize, Vec<Block>>>,

    torrent_info: HashMap<InfoHash, Info>,

    /// The block infos of each piece of a torrent, ordered from 0 to last.
    pieces_blocks: HashMap<InfoHash, BTreeMap<usize, Vec<BlockInfo>>>,

    torrent_cache: HashMap<InfoHash, TorrentCache>,

    file_handle_cache: LruCache<PathBuf, tokio::fs::File>,

    rx: Receiver<DiskMsg>,
}

impl Disk {
    pub fn new(download_dir: String) -> Self {
        let (tx, rx) = mpsc::channel::<DiskMsg>(100);

        Self {
            rx,
            tx,
            download_dir,
            file_handle_cache: LruCache::new(NonZeroUsize::new(10).unwrap()),
            torrent_cache: HashMap::new(),
            cache: HashMap::new(),
            peer_ctxs: Vec::new(),
            torrent_ctxs: HashMap::new(),
            downloaded_pieces_len: HashMap::new(),
            piece_strategy: HashMap::default(),
            pieces_blocks: HashMap::default(),
            torrent_info: HashMap::default(),
            piece_order: HashMap::default(),
        }
    }

    #[tracing::instrument(name = "disk", skip_all)]
    pub async fn run(&mut self) -> Result<(), Error> {
        debug!("disk started event loop");

        while let Some(msg) = self.rx.recv().await {
            match msg {
                DiskMsg::DeleteTorrent(info_hash) => {
                    self.torrent_cache.remove_entry(&info_hash);
                    self.cache.remove_entry(&info_hash);
                    self.torrent_ctxs.remove_entry(&info_hash);
                    self.downloaded_pieces_len.remove_entry(&info_hash);
                    self.piece_strategy.remove_entry(&info_hash);
                    self.pieces_blocks.remove_entry(&info_hash);
                    self.torrent_info.remove_entry(&info_hash);
                    self.piece_order.remove_entry(&info_hash);
                    self.peer_ctxs.retain(|v| v.info_hash != info_hash);
                }
                DiskMsg::DeletePeer(addr) => {
                    trace!("delete_peer {addr:?}");
                    self.delete_peer(addr);
                }
                DiskMsg::NewTorrent(torrent) => {
                    info!("new_torrent");
                    let _ = self.new_torrent(torrent).await;
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
                DiskMsg::RequestBlocks {
                    qnt,
                    recipient,
                    info_hash,
                    peer_id,
                    peer_pieces,
                } => {
                    let infos = self
                        .request_blocks(&info_hash, &peer_id, peer_pieces, qnt)
                        .await
                        .unwrap_or_default();

                    info!("disk sending {} block infos", infos.len());

                    let _ = recipient.send(infos);
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
                DiskMsg::ReturnBlockInfos(info_hash, block_infos) => {
                    info!("return_block_infos");

                    for block in block_infos {
                        // get vector of piece_blocks for each
                        // piece of the blocks.
                        self.pieces_blocks
                            .get_mut(&info_hash)
                            .ok_or(Error::TorrentDoesNotExist)?
                            .entry(block.index as usize)
                            .or_default()
                            .push(block);
                    }
                }
                DiskMsg::Quit => {
                    debug!("Quit");
                    return Ok(());
                }
            }
        }
        info!("disk leaving fn run");

        Ok(())
    }

    async fn get_cached_file(
        &mut self,
        path: &Path,
    ) -> Result<tokio::fs::File, Error> {
        if let Some(file) = self.file_handle_cache.get_mut(path) {
            return Ok(file.try_clone().await?);
        }

        let file = Self::open_file(path).await?;

        if let Ok(cloned) = file.try_clone().await {
            self.file_handle_cache.put(path.to_path_buf(), cloned);
        }

        Ok(file)
    }

    async fn preallocate_files(
        &mut self,
        info_hash: &InfoHash,
    ) -> Result<(), Error> {
        if let Some(cache) = self.torrent_cache.get(info_hash) {
            for meta in &cache.file_metadata {
                let path = self.base_path(info_hash).join(&meta.path);

                if let Some(parent) = path.parent() {
                    tokio::fs::create_dir_all(parent).await?;
                }

                if let Ok(file) = Self::open_file(&path).await {
                    file.set_len(meta.length).await?;
                }
            }
        }
        Ok(())
    }

    // pub async fn new_torrent_two(
    //     &mut self,
    //     torrent_ctx: Arc<TorrentCtx>,
    //     info_hash: InfoHash,
    // ) -> Result<(), Error> {
    //     self.torrent_ctxs.insert(info_hash, torrent_ctx.clone());
    //     Ok(())
    // }

    /// Initialize necessary data.
    pub async fn new_torrent(
        &mut self,
        torrent_ctx: Arc<TorrentCtx>,
    ) -> Result<(), Error> {
        let info_hash = &torrent_ctx.info_hash;
        let info_ = torrent_ctx.info.read().await;
        let info = info_.clone();
        drop(info_);

        let total_size = info.get_size();
        let piece_length = info.piece_length as u64;

        self.torrent_ctxs.insert(info_hash.clone(), torrent_ctx.clone());
        self.torrent_info.insert(info_hash.clone(), info.clone());

        let mut file_metadata = Vec::new();
        let mut current_offset = 0;

        if let Some(files) = &info.files {
            for file in files {
                file_metadata.push(FileMetadata {
                    path: PathBuf::from_iter(&file.path),
                    start_offset: current_offset,
                    length: file.length,
                });
                current_offset += file.length;
            }
        } else {
            file_metadata.push(FileMetadata {
                path: PathBuf::from(&info.name),
                start_offset: 0,
                length: info.file_length.unwrap_or(0),
            });
        }

        self.torrent_cache.insert(
            info_hash.clone(),
            TorrentCache { file_metadata, piece_length, total_size },
        );

        // create folders, files, and preallocate them.
        self.preallocate_files(info_hash).await?;

        self.piece_strategy.insert(info_hash.clone(), PieceStrategy::default());

        let mut all_pieces: Vec<u32> = (0..info.pieces()).collect();

        // let piece_order =
        // self.piece_strategy.get(info_hash).cloned().unwrap();
        // if piece_order != PieceStrategy::Sequential {
        all_pieces.shuffle(&mut rand::rng());
        // }

        self.piece_order.insert(info_hash.clone(), all_pieces);
        self.cache.insert(info_hash.clone(), BTreeMap::new());
        self.pieces_blocks.insert(info_hash.clone(), info.get_block_infos()?);
        self.downloaded_pieces_len.insert(info_hash.clone(), 0);

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
            let _ = ctx.tx.send(PeerMsg::GetPieces(otx)).await;

            let Some(pieces) = orx.await.ok() else {
                continue;
            };

            for (i, item) in pieces.iter().enumerate() {
                // increment each occurence of a piece
                if *item {
                    if let Some(item) = score.get_mut(i) {
                        *item += 1;
                    }
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

        let piece_order = self.piece_strategy.get_mut(info_hash).unwrap();
        if *piece_order == PieceStrategy::Random {
            *piece_order = PieceStrategy::Rarest;
        }

        Ok(())
    }

    /// Request available block infos following the order of PieceStrategy.
    pub async fn request_blocks(
        &mut self,
        info_hash: &InfoHash,
        _peer_id: &PeerId,
        peer_pieces: Bitfield,
        qnt: usize,
    ) -> Result<Vec<BlockInfo>, Error> {
        if peer_pieces.is_empty() {
            warn!("peer_pieces is empty");
            return Ok(vec![]);
        }

        // let Some(peer_ctx) = self.peer_ctxs.iter().find(|v| v.id == *peer_id)
        // else {
        //     warn!("peer not found: {peer_id:?}");
        //     return Ok(vec![]);
        // };

        let torrent_ctx = self
            .torrent_ctxs
            .get(info_hash)
            .ok_or(Error::TorrentDoesNotExist)?
            .clone();

        // info!("peer_pieces len {}", peer_pieces.len());
        // info!("peer_pieces ones {}", peer_pieces.count_ones());

        // --------
        let (otx, orx) = oneshot::channel();
        torrent_ctx.tx.send(TorrentMsg::ReadBitfield(otx)).await?;
        let local_pieces = orx.await?;
        // info!("request_blocks local_pieces {}", local_pieces.len());
        // --------

        // --------
        // let (otx, orx) = oneshot::channel();
        // peer_ctx.tx.send(PeerMsg::GetPieces(otx)).await?;
        // let peer_pieces = orx.await?;
        // info!("request_blocks peer_pieces len {}", peer_pieces.len());
        // --------

        // order of pieces to download
        let piece_order = self
            .piece_order
            .get(info_hash)
            .ok_or(Error::TorrentDoesNotExist)?;

        let mut result: Vec<BlockInfo> = Vec::with_capacity(qnt);

        for piece in 0..piece_order.len() {
            // get a piece that the remote peer has...
            let Some(_p @ true) = peer_pieces.get(piece).as_deref() else {
                continue;
            };

            // but the local peer does not.
            let Some(_p @ false) = local_pieces.get(piece).as_deref() else {
                continue;
            };

            // block infos of the current piece
            let Some(mut block_infos) = self
                .pieces_blocks
                .get_mut(info_hash)
                .ok_or(Error::TorrentDoesNotExist)?
                .remove(&piece)
            // todo: maybe also delete self.peers if piece was fully requested ?
            else {
                continue;
            };

            let to_add: Vec<BlockInfo> =
                block_infos.drain(0..qnt.min(block_infos.len())).collect();

            result.extend(to_add);

            // if not all blocks were pushed to result, put them back
            if !block_infos.is_empty() {
                self.pieces_blocks
                    .get_mut(info_hash)
                    .ok_or(Error::TorrentDoesNotExist)?
                    .insert(piece, block_infos);
            }

            if result.len() >= qnt {
                break;
            }
        }

        Ok(result)
    }

    /// Open a file given a path, the path is absolute
    /// and does not consider the base path of the torrent,
    /// if this behaviour is wanted, you can get the base path
    /// of the torrent using `base_path`.
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

    // pub async fn maintain_cache(&mut self) {
    //     let now = Instant::now();
    //     self.file_handle_cache.iter_mut().for_each(|(_, file)| {
    //         if now.duration_since(file.last_accessed) >
    // Duration::from_secs(300) {             file.cleanup().await;  //
    // Custom flush/close logic         }
    //     });
    //
    //     let new_size = todo!();
    //     self.file_handle_cache.resize(new_size);
    // }

    pub async fn read_block(
        &mut self,
        info_hash: &InfoHash,
        block_info: &BlockInfo,
    ) -> Result<Block, Error> {
        // Get torrent metadata
        let cache = self
            .torrent_cache
            .get(info_hash)
            .ok_or(Error::TorrentDoesNotExist)?;

        // Calculate absolute offset
        let absolute_offset = block_info.index as u64 * cache.piece_length
            + block_info.begin as u64;

        // Find containing file
        let file_meta = cache
            .file_metadata
            .iter()
            .find(|m| {
                absolute_offset >= m.start_offset
                    && absolute_offset < m.start_offset + m.length
            })
            .ok_or(Error::TorrentDoesNotExist)?;

        // Calculate file-relative offset
        let file_offset = absolute_offset - file_meta.start_offset;

        // Build full path
        let mut path = self.base_path(info_hash);
        path.extend(&file_meta.path);

        // Get cached file handle
        let mut file = self.get_cached_file(&path).await?;

        // Read data
        let mut buf = vec![0; block_info.len as usize];

        file.seek(SeekFrom::Start(file_offset)).await?;

        file.read_exact(&mut buf).await?;

        let torrent_ctx = self
            .torrent_ctxs
            .get(info_hash)
            .ok_or(Error::TorrentDoesNotExist)?
            .clone();

        // we change perspectives here because those values are
        // going to be sent to the tracker in our perspective.
        let _ = torrent_ctx
            .tx
            .send(TorrentMsg::IncrementUploaded(block_info.len as u64))
            .await;

        Ok(Block {
            index: block_info.index as usize,
            begin: block_info.begin,
            block: buf,
        })
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
            .cache
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

        // we change perspectives here because those values are
        // going to be sent to the tracker in our perspective.
        let _ = torrent_ctx
            .tx
            .send(TorrentMsg::IncrementDownloaded(len as u64))
            .await;

        // continue function if the piece was fully downloaded
        if self
            .cache
            .get(info_hash)
            .ok_or(Error::TorrentDoesNotExist)?
            .get(&index)
            .ok_or(Error::TorrentDoesNotExist)?
            .iter()
            .fold(0, |acc, v| acc + v.block.len())
            < self.piece_size(info_hash, index)? as usize
        {
            return Ok(());
        }

        let downloaded_pieces_len = self
            .downloaded_pieces_len
            .get_mut(info_hash)
            .ok_or(Error::TorrentDoesNotExist)?;

        *downloaded_pieces_len += 1;

        // validate that the downloaded pieces hash
        // matches the hash of the info.
        if let Err(e) = self.validate_piece(info_hash, index).await {
            warn!("piece {index} is corrupted.");
            return Err(e);
        }

        debug!("piece {index} is valid.");

        let _ = torrent_ctx.tx.send(TorrentMsg::SetBitfield(index)).await;
        let _ = torrent_ctx.tx.send(TorrentMsg::DownloadedPiece(index)).await;

        // at this point the piece is valid,
        // get the file path of all the blocks,
        // and then write all bytes into the files.
        self.write_pieces(info_hash, index).await?;

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

        let pieces = &self
            .torrent_ctxs
            .get(info_hash)
            .ok_or(Error::TorrentDoesNotExist)?
            .info
            .read()
            .await
            .pieces;

        let hash_from_info = pieces[b..e].to_owned();

        let blocks = self
            .cache
            .get_mut(info_hash)
            .ok_or(Error::TorrentDoesNotExist)?
            .get_mut(&index)
            .ok_or(Error::TorrentDoesNotExist)?;

        blocks.sort();

        let mut hasher = sha1_smol::Sha1::new();
        for block in blocks {
            hasher.update(&block.block);
        }

        let hash = hasher.digest().bytes();

        if *hash_from_info != hash {
            return Err(Error::PieceInvalid);
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

            // Build full path
            let mut path = self.base_path(info_hash);
            path.extend(&file_meta.path);

            write_ops.push((path, file_offset, buffer_start..buffer_end));
        }

        Ok(write_ops)
    }

    /// Write all cached blocks of `piece` to disk.
    /// It will free the blocks in the cache.
    pub async fn write_pieces(
        &mut self,
        info_hash: &InfoHash,
        piece_index: usize,
    ) -> Result<(), Error> {
        // Get blocks from cache
        let blocks = self
            .cache
            .get_mut(info_hash)
            .and_then(|c| c.remove(&piece_index))
            .ok_or(Error::PieceInvalid)?;

        let total_length = blocks.iter().map(|b| b.block.len()).sum();

        // Combine blocks into single contiguous buffer
        let mut piece_buffer = Vec::with_capacity(total_length);

        for mut block in blocks {
            piece_buffer.append(&mut block.block);
        }

        // Calculate write operations
        let write_ops =
            self.calculate_write_ops(info_hash, piece_index, &piece_buffer)?;

        // Group writes by file
        let mut file_ops: HashMap<
            PathBuf,
            Vec<(u64, std::ops::Range<usize>, tokio::fs::File)>,
        > = HashMap::new();

        for (path, file_offset, data_range) in write_ops {
            file_ops.entry(path.clone()).or_default().push((
                file_offset,
                data_range,
                self.get_cached_file(&path).await?,
            ));
        }

        let mut tasks = Vec::with_capacity(file_ops.len());

        for (_path, ops) in file_ops {
            let piece_buffer = piece_buffer.clone();
            // let path_clone = path.clone();

            tasks.push(tokio::spawn(async move {
                // let mut file = Self::open_file(&path_clone).await?;

                // Sort ops by offset for sequential write
                let mut ops = ops;
                ops.sort_by_key(|(offset, _, _)| *offset);

                for (file_offset, data_range, mut file) in ops {
                    file.seek(SeekFrom::Start(file_offset)).await?;
                    file.write_all(&piece_buffer[data_range]).await?;
                }

                Ok::<(), Error>(())
            }));
        }

        join_all(tasks).await;

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
            if remainder == 0 {
                info.piece_length
            } else {
                remainder as u32
            }
        } else {
            info.piece_length
        })
    }
    /// Get the base path of a torrent directory.
    /// Which is always "download_dir/name_of_torrent".
    pub fn base_path(&self, info_hash: &InfoHash) -> PathBuf {
        let info = self.torrent_info.get(info_hash).unwrap();
        let mut base = PathBuf::from(&self.download_dir);
        base.push(&info.name);
        base
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use futures::StreamExt;
    use rand::{distr::Alphanumeric, Rng};
    use std::net::{Ipv4Addr, SocketAddrV4};
    use tokio::{
        sync::{Mutex, RwLock},
        time::Instant,
    };
    use tokio_util::codec::Framed;

    use crate::{
        counter::Counter,
        daemon::Daemon,
        extensions::{core::BLOCK_LEN, CoreCodec},
        magnet::Magnet,
        metainfo::{self, Info},
        peer::{self, Peer, StateLog, DEFAULT_REQUEST_QUEUE_LEN},
        torrent::{Connected, Stats, Torrent},
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
        let root_dir = format!("/tmp/{download_dir}/{torrent_dir}");
        println!("{root_dir}");

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

        let magnet = Magnet::new(&magnet).unwrap();
        let mut disk = Disk::new(format!("/tmp/{download_dir}"));
        let disk_tx = disk.tx.clone();
        let mut daemon = Daemon::new(disk_tx.clone());
        let daemon_ctx = daemon.ctx.clone();
        let _daemon_tx = daemon_ctx.tx.clone();

        let (tracker_tx, _tracker_rx) = mpsc::channel::<TrackerMsg>(100);
        let (torrent_tx, torrent_rx) = mpsc::channel::<TorrentMsg>(100);

        let mut hasher = sha1_smol::Sha1::new();
        hasher.update(&[7u8; (BLOCK_LEN) as usize]);
        let hash = hasher.digest().bytes();

        let mut pieces = vec![];
        for _ in 0..6 {
            pieces.extend(hash);
        }

        let mut info = Info {
            piece_length: BLOCK_LEN,
            pieces,
            name: torrent_dir.clone(),
            file_length: None,
            files: Some(files.clone()),
            metadata_size: None,
        };

        let pieces_len = info.pieces();

        let info_hash = magnet.parse_xt_infohash();

        let metadata_size = info.metadata_size()?;
        info.metadata_size = Some(metadata_size);

        let info = RwLock::new(info);

        let torrent_ctx = Arc::new(TorrentCtx {
            disk_tx: disk_tx.clone(),
            tx: torrent_tx,
            magnet: magnet.clone(),
            info_hash: info_hash.clone(),
            info,
        });

        let (peer_tx, peer_rx) = mpsc::channel::<PeerMsg>(100);

        let peer_ctx = Arc::new(PeerCtx {
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
            uploaded: 0.into(),
            downloaded: 0.into(),
            id: PeerId::gen(),
            am_interested: true.into(),
            am_choking: false.into(),
            peer_choking: false.into(),
            direction: peer::Direction::Outbound,
        });

        let listener = TcpListener::bind("0.0.0.0:0").await?;
        let local_addr = listener.local_addr()?;

        let pieces =
            bitvec::bitvec![u8, bitvec::prelude::Msb0; 1; pieces_len as usize];

        let peer_ctx_ = peer_ctx.clone();
        let torrent_ctx_ = torrent_ctx.clone();

        let stream = TcpStream::connect(local_addr).await?;

        let mut torrent = Torrent {
            state: Connected {
                size: 0,
                last_rate_update: Instant::now(),
                counter: Counter::new(),
                unchoked_peers: Vec::new(),
                opt_unchoked_peer: None,
                connecting_peers: Vec::new(),
                error_peers: Vec::new(),
                downloaded_info_bytes: metadata_size,
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
                have_info: true,
                info_pieces: BTreeMap::new(),
            },
            ctx: torrent_ctx.clone(),
            daemon_ctx,
            name: torrent_dir.clone(),
            rx: torrent_rx,
            status: crate::torrent::TorrentStatus::Downloading,
        };

        disk.new_peer(peer_ctx.clone());
        disk.new_torrent(torrent_ctx).await?;

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

        let peer_pieces = pieces.clone();

        spawn(async move {
            let (_socket, _) = listener.accept().await.unwrap();
            let socket = Framed::new(stream, CoreCodec);
            let (sink, stream) = socket.split();

            let mut peer = Peer::<peer::Connected> {
                state_log: StateLog::default(),
                state: peer::Connected {
                    is_paused: false,
                    connection: peer::ConnectionState::default(),
                    ctx: peer_ctx_,
                    ext_states: peer::ExtStates::default(),
                    have_info: true,
                    in_endgame: false,
                    incoming_requests: Vec::new(),
                    outgoing_requests: Vec::new(),
                    outgoing_requests_info_pieces: Vec::new(),
                    pieces: peer_pieces,
                    reserved: bitvec::bitarr![u8, bitvec::prelude::Msb0; 0; 8 * 8],
                    rx: peer_rx,
                    seed_only: false,
                    sink,
                    stream,
                    target_request_queue_len: DEFAULT_REQUEST_QUEUE_LEN,
                    torrent_ctx: torrent_ctx_,
                },
            };

            let _ = peer.run().await;
        });

        let (otx, orx) = oneshot::channel();
        disk_tx
            .send(DiskMsg::RequestBlocks {
                info_hash: info_hash.clone(),
                peer_id: peer_ctx.id.clone(),
                recipient: otx,
                qnt: 3,
                peer_pieces: pieces,
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

        let _ = tokio::fs::remove_dir_all(format!("/tmp/{download_dir}")).await;

        Ok(())
    }
}
