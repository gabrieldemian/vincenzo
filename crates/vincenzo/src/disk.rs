//! Disk is responsible for file I/O of all Torrents.
use std::{
    collections::BTreeMap,
    io::SeekFrom,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
};

use hashbrown::HashMap;
use rand::seq::SliceRandom;
use tokio::{
    fs::{create_dir_all, File, OpenOptions},
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
    sync::{
        mpsc::{self, Receiver},
        oneshot::{self, Sender},
    },
};
use tracing::{debug, info, warn};

use crate::{
    error::Error,
    extensions::core::{Block, BlockInfo},
    metainfo::{self, Info},
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
    ReadBlock {
        info_hash: InfoHash,
        block_info: BlockInfo,
        recipient: Sender<Vec<u8>>,
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
    OpenFile(String, Sender<File>),
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
    pub pieces: HashMap<InfoHash, Vec<u32>>,

    /// How many pieces were downloaded.
    pub downloaded_pieces_len: HashMap<InfoHash, u32>,

    pub piece_strategy: HashMap<InfoHash, PieceStrategy>,

    pub download_dir: String,

    /// A cache of blocks, where the key is a piece.
    cache: HashMap<InfoHash, BTreeMap<usize, Vec<Block>>>,

    torrent_info: HashMap<InfoHash, Info>,

    /// The block infos of each piece of a torrent, ordered from 0 to last.
    pieces_blocks: HashMap<InfoHash, BTreeMap<usize, Vec<BlockInfo>>>,

    rx: Receiver<DiskMsg>,
}

impl Disk {
    pub fn new(download_dir: String) -> Self {
        let (tx, rx) = mpsc::channel::<DiskMsg>(100);

        Self {
            rx,
            tx,
            download_dir,
            cache: HashMap::new(),
            peer_ctxs: Vec::new(),
            torrent_ctxs: HashMap::new(),
            downloaded_pieces_len: HashMap::new(),
            piece_strategy: HashMap::default(),
            pieces_blocks: HashMap::default(),
            torrent_info: HashMap::default(),
            pieces: HashMap::default(),
        }
    }

    #[tracing::instrument(skip(self), name = "disk::run")]
    pub async fn run(&mut self) -> Result<(), Error> {
        debug!("disk started event loop");

        while let Some(msg) = self.rx.recv().await {
            match msg {
                DiskMsg::DeletePeer(addr) => {
                    debug!("DeletePeer {addr:?}");
                    self.delete_peer(addr);
                }
                DiskMsg::NewTorrent(torrent) => {
                    println!("new_torrent");
                    let _ = self.new_torrent(torrent).await;
                }
                DiskMsg::ReadBlock { block_info, recipient, info_hash } => {
                    debug!("ReadBlock");

                    let len = block_info.len;

                    let bytes = self.read_block(&info_hash, block_info).await?;
                    let _ = recipient.send(bytes);

                    // increment uploaded count
                    let tx = &self.torrent_ctxs.get(&info_hash).unwrap().tx;
                    tx.send(TorrentMsg::IncrementUploaded(len as u64)).await?;
                }
                DiskMsg::WriteBlock { block, info_hash } => {
                    debug!("WriteBlock");
                    self.write_block(&info_hash, block).await?;
                }
                DiskMsg::OpenFile(path, tx) => {
                    debug!("OpenFile");
                    let file = Self::open_file(path).await?;
                    let _ = tx.send(file);
                }
                DiskMsg::RequestBlocks {
                    qnt,
                    recipient,
                    info_hash,
                    peer_id,
                } => {
                    let infos = self
                        .request_blocks(&info_hash, &peer_id, qnt)
                        .await
                        .unwrap_or_default();

                    info!("disk sending {}", infos.len());

                    let _ = recipient.send(infos);
                }
                DiskMsg::ValidatePiece { info_hash, recipient, piece } => {
                    debug!("ValidatePiece");
                    let r = self.validate_piece(&info_hash, piece).await;
                    let _ = recipient.send(r);
                }
                DiskMsg::NewPeer(peer) => {
                    self.new_peer(peer);
                }
                DiskMsg::ReturnBlockInfos(info_hash, block_infos) => {
                    debug!("ReturnBlockInfos");
                    for block in block_infos {
                        // get vector of piece_blocks for each
                        // piece of the blocks.
                        if let Some(piece) = self
                            .pieces_blocks
                            .get_mut(&info_hash)
                            .ok_or(Error::TorrentDoesNotExist)?
                            .get_mut(&(block.index as usize))
                        {
                            piece.push(block);
                        }
                    }
                }
                DiskMsg::Quit => {
                    debug!("Quit");
                    return Ok(());
                }
            }
        }

        Ok(())
    }

    /// Create a file tree with a given root directory, but doesn't allocate
    /// eagerly.
    pub async fn create_file_tree(
        // Root dir in which to create the file tree. If this is a single file
        // torrent, the root will only be the download_dir, otherwise
        // : torrent_name + download_dir
        root_dir: &PathBuf,
        meta_files: &Vec<metainfo::File>,
    ) -> Result<(), Error> {
        for meta_file in meta_files {
            if meta_file.path.is_empty() {
                continue;
            };

            let mut path = PathBuf::from(root_dir);

            // the last item of the vec will be a file, all the previous ones
            // will be directories.
            let Some(file) = meta_file.path.last() else { continue };

            // get an array of the dirs.
            let Some(dirs) = meta_file.path.get(0..meta_file.path.len() - 1)
            else {
                continue;
            };

            path.extend(dirs);
            create_dir_all(&path).await?;

            path.push(file);
            Self::open_file(path).await?;
        }
        Ok(())
    }

    /// Initialize necessary data.
    ///
    /// # Important
    /// Must only be called after torrent has Info downloaded
    #[tracing::instrument(skip(self, torrent_ctx), name = "new_torrent")]
    pub async fn new_torrent(
        &mut self,
        torrent_ctx: Arc<TorrentCtx>,
    ) -> Result<(), Error> {
        let info_hash = &torrent_ctx.info_hash;
        let info_ = torrent_ctx.info.read().await;
        let info = info_.clone();
        drop(info_);

        self.torrent_ctxs.insert(info_hash.clone(), torrent_ctx.clone());
        self.torrent_info.insert(info_hash.clone(), info.clone());

        let base = self.base_path(info_hash);
        let pieces_len = info.pieces();

        let files = match &info.files {
            Some(files) => files.clone(),
            None => vec![metainfo::File {
                path: vec![info.name.clone()],
                length: info.file_length.unwrap(),
            }],
        };

        // create "skeleton" of the torrent, empty files and directories
        Self::create_file_tree(&base, &files).await?;

        self.piece_strategy.insert(info_hash.clone(), PieceStrategy::default());
        let piece_order = self.piece_strategy.get(info_hash).cloned().unwrap();

        let mut all_pieces: Vec<u32> = (0..pieces_len).collect();

        if piece_order != PieceStrategy::Sequential {
            all_pieces.shuffle(&mut rand::rng());
        }

        info!("shuffled pieces {all_pieces:?}");

        self.pieces.insert(info_hash.clone(), all_pieces);
        self.cache.insert(info_hash.clone(), BTreeMap::new());

        // generate all block_infos of this torrent
        let blocks = info.get_block_infos()?;

        let pieces_blocks =
            self.pieces_blocks.entry(info_hash.clone()).or_default();

        *pieces_blocks = blocks;

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
        let pieces =
            self.pieces.get_mut(info_hash).ok_or(Error::TorrentDoesNotExist)?;

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
    #[tracing::instrument(skip_all)]
    pub async fn request_blocks(
        &mut self,
        info_hash: &InfoHash,
        peer_id: &PeerId,
        qnt: usize,
    ) -> Result<Vec<BlockInfo>, Error> {
        let mut result: Vec<BlockInfo> = Vec::with_capacity(qnt);

        let peer_ctx = self
            .peer_ctxs
            .iter()
            .find(|v| v.id == *peer_id)
            .ok_or(Error::PeerNotFound(peer_id.clone()))?;

        let (otx, orx) = oneshot::channel();
        let _ = peer_ctx.tx.send(PeerMsg::GetPieces(otx)).await;

        // pieces that the remote peer has
        let peer_pieces = orx.await?;

        // order of pieces to download
        let pieces =
            self.pieces.get(info_hash).ok_or(Error::TorrentDoesNotExist)?;

        let torrent_ctx = self
            .torrent_ctxs
            .get(info_hash)
            .ok_or(Error::TorrentDoesNotExist)?
            .clone();

        let (otx, orx) = oneshot::channel();
        torrent_ctx.tx.send(TorrentMsg::ReadBitfield(otx)).await?;

        // pieces that the client has
        let local_pieces = orx.await?;
        println!("local pieces {}", local_pieces);
        println!("peer pieces  {}", peer_pieces);

        for piece in 0..pieces.len() {
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
                println!("appending");
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

    pub async fn read_block(
        &self,
        info_hash: &InfoHash,
        block_info: BlockInfo,
    ) -> Result<Vec<u8>, Error> {
        let mut file =
            self.get_file_from_block_info(&block_info, info_hash).await?;

        // how many bytes to read, after offset (begin)
        let mut buf = vec![0; block_info.len as usize];

        file.0.read_exact(&mut buf).await?;

        Ok(buf)
    }

    /// Write block to a cache of pieces, when a piece has been fully
    /// downloaded and validated, write it to disk and clear the cache.
    ///
    /// If the download algorithm of the pieces is "Random", and it has
    /// downloaded the first piece, change the algorithm to rarest-first.
    #[tracing::instrument(skip(self, block))]
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

        self.cache
            .get_mut(info_hash)
            .ok_or(Error::TorrentDoesNotExist)?
            .get_mut(&index)
            .ok_or(Error::TorrentDoesNotExist)?
            .push(block);

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

        self.rarest_first(info_hash).await?;

        let _ = torrent_ctx
            .tx
            .send(TorrentMsg::IncrementDownloaded(len as u64))
            .await;

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

    /// Return a seeked tokio::fs::File, given a `BlockInfo`.
    ///
    /// # Use cases:
    /// - After we receive a Piece msg, we need to map the block to a fs::File
    ///   to be able to write to disk.
    ///
    /// - When a leecher sends a Request msg, we need to get the corresponding
    ///   file seeked on the right offset of the block info.
    pub async fn get_file_from_block_info(
        &self,
        block_info: &BlockInfo,
        info_hash: &InfoHash,
    ) -> Result<(File, metainfo::File), Error> {
        let torrent =
            self.torrent_ctxs.get(info_hash).ok_or(Error::InfoHashInvalid)?;

        let info = torrent.info.read().await;

        let absolute_offset = block_info.index as u64
            * info.piece_length as u64
            + block_info.begin as u64;

        let mut path = self.base_path(info_hash);

        if let Some(files) = &info.files {
            let mut accumulated_length = 0_u64;

            for file_info in files.iter() {
                if accumulated_length + file_info.length as u64
                    > absolute_offset
                {
                    path.extend(&file_info.path);

                    let mut file = Self::open_file(&path).await?;

                    let file_relative_offset =
                        absolute_offset - accumulated_length;

                    file.seek(SeekFrom::Start(file_relative_offset)).await?;

                    return Ok((file, file_info.clone()));
                }
                accumulated_length += file_info.length as u64;
            }

            Err(Error::FileOpenError("Offset exceeds file sizes".to_owned()))
        } else {
            let mut file = Self::open_file(path).await?;
            file.seek(SeekFrom::Start(absolute_offset)).await?;

            let file_info = metainfo::File {
                path: vec![info.name.to_owned()],
                length: info.file_length.unwrap(),
            };

            Ok((file, file_info))
        }
    }

    /// Given a piece, find it's corresponding file.
    /// The file will NOT be seeked.
    pub async fn get_file_from_piece(
        &self,
        piece: u32,
        info_hash: &InfoHash,
    ) -> Result<metainfo::File, Error> {
        let torrent =
            self.torrent_ctxs.get(info_hash).ok_or(Error::InfoHashInvalid)?;

        let info = torrent.info.read().await;
        let piece_len = info.piece_length;

        // multi file torrent
        if let Some(files) = &info.files {
            let mut acc = 0;
            let file_info = files.iter().find(|f| {
                let pieces = f.pieces(piece_len) as u64 + acc;
                acc = pieces;
                piece as u64 >= pieces
            });

            let file = file_info.ok_or(Error::FileOpenError("".to_owned()))?;

            return Ok(file.clone());
        }

        // single file torrent
        let file_info = metainfo::File {
            path: vec![info.name.to_owned()],
            length: info.file_length.unwrap(),
        };

        Ok(file_info)
    }

    /// Given a `BlockInfo`, find the corresponding `Block`
    /// by reading the disk.
    pub async fn get_block_from_block_info(
        &self,
        block_info: &BlockInfo,
        info_hash: &InfoHash,
    ) -> Result<Block, Error> {
        // todo: try to get the block from cache first,
        // if not in cache, read from disk.
        let mut file =
            self.get_file_from_block_info(block_info, info_hash).await?;

        let mut buf = vec![0; block_info.len as usize];

        file.0.read_exact(&mut buf).await.unwrap();

        let block = Block {
            index: block_info.index as usize,
            begin: block_info.begin,
            block: buf,
        };

        Ok(block)
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

        let mut hash = sha1_smol::Sha1::new();

        let blocks = self
            .cache
            .get_mut(info_hash)
            .ok_or(Error::TorrentDoesNotExist)?
            .get_mut(&index)
            .ok_or(Error::TorrentDoesNotExist)?;

        blocks.sort();

        for block in blocks {
            hash.update(&block.block);
        }

        let hash = hash.digest().bytes();

        if hash_from_info != hash {
            return Err(Error::PieceInvalid);
        }

        Ok(())
    }

    /// Write all cached blocks of `piece` to disk.
    /// It will free the blocks in the cache.
    async fn write_pieces(
        &mut self,
        info_hash: &InfoHash,
        piece: usize,
    ) -> Result<(), Error> {
        let mut blocks: Vec<Block> = self
            .cache
            .get_mut(info_hash)
            .ok_or(Error::TorrentDoesNotExist)?
            .get_mut(&piece)
            .ok_or(Error::TorrentDoesNotExist)?
            .drain(..)
            .collect();

        blocks.sort();

        let info = self
            .torrent_info
            .get(info_hash)
            .ok_or(Error::TorrentDoesNotExist)?;

        let files = match &info.files {
            Some(files) => files.clone(),
            None => vec![metainfo::File {
                path: vec![info.name.clone()],
                length: info.file_length.unwrap(),
            }],
        };

        let piece_length = info.piece_length as u64;
        let piece_offset = piece as u64 * piece_length;

        let mut file_to_blocks: HashMap<PathBuf, Vec<Block>> = HashMap::new();

        // Distribute blocks to corresponding files
        for block in blocks {
            let block_info = BlockInfo {
                index: block.index as u32,
                begin: block.begin,
                len: block.block.len() as u32,
            };

            // get the file path of this block
            let (mut _file, mt_file) =
                self.get_file_from_block_info(&block_info, info_hash).await?;

            let mut file_path = PathBuf::new();
            file_path.extend(mt_file.path);

            file_to_blocks.entry(file_path).or_default().push(block);
        }

        // Write the blocks to their corresponding file
        for (file_path_buf, blocks) in file_to_blocks {
            let mut bytes: Vec<u8> = Vec::new();

            for block in &blocks {
                bytes.extend_from_slice(&block.block);
            }

            // Construct the file path
            let mut full_file_path = self.base_path(info_hash);
            full_file_path.extend(&file_path_buf);

            let mut file = Self::open_file(&full_file_path).await?;

            // The accumulated length of all the files of the torrent
            // up to the current file.
            let file_len = files
                .iter()
                .find(|f| PathBuf::from_iter(&f.path) == file_path_buf)
                .map(|v| v.length)
                .unwrap();

            let file_offset = piece_offset.saturating_sub(file_len as u64);

            debug!("file_offset {file_offset}");

            file.seek(SeekFrom::Start(file_offset)).await?;
            file.write_all(&bytes).await?;
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
    use std::{
        collections::BTreeMap,
        net::{Ipv4Addr, SocketAddrV4},
        time::Duration,
    };

    use futures::StreamExt;
    use rand::{distr::Alphanumeric, Rng};
    use tokio_util::codec::Framed;

    use crate::{
        bitfield::Bitfield,
        daemon::Daemon,
        extensions::{core::BLOCK_LEN, CoreCodec},
        magnet::Magnet,
        metainfo::{self, Info},
        peer::{self, Peer},
        torrent::{Connected, Stats, Torrent},
        tracker::{TrackerCtx, TrackerMsg},
    };

    use super::*;
    use tokio::{
        net::{TcpListener, TcpStream},
        spawn,
        sync::{mpsc, RwLock},
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
                length: BLOCK_LEN * 2,
                path: vec!["foo.txt".to_owned()],
            },
            metainfo::File {
                length: BLOCK_LEN * 2,
                path: vec!["bar".to_owned(), "baz.txt".to_owned()],
            },
            metainfo::File {
                length: BLOCK_LEN * 2,
                path: vec![
                    "bar".to_owned(),
                    "buzz".to_owned(),
                    "bee.txt".to_owned(),
                ],
            },
        ];

        Disk::create_file_tree(&PathBuf::from(&root_dir), &files).await?;

        for df in &files {
            let mut path = PathBuf::from(&root_dir);
            path.extend(&df.path);
            assert!(Path::new(&path).is_file());

            // let mut file = Disk::open_file(&path).await?;
            // file.write_all(&[7u8; (BLOCK_LEN * 2) as usize]).await?;
            // file.flush().await?;
        }
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
                unchoked_peers: Vec::new(),
                opt_unchoked_peer: None,
                connecting_peers: Vec::new(),
                error_peers: Vec::new(),
                downloaded_info_bytes: metadata_size,
                bitfield: bitvec::bitvec![u8, bitvec::prelude::Msb0; 0; pieces_len as usize],
                stats: Stats { seeders: 0, leechers: 0, interval: 1000 },
                idle_peers: vec![],
                tracker_ctx: Arc::new(TrackerCtx {
                    downloaded: 0,
                    uploaded: 0,
                    left: 0,
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
                download_rate: 0,
                last_second_downloaded: 0,
            },
            ctx: torrent_ctx.clone(),
            daemon_ctx,
            name: torrent_dir.clone(),
            rx: torrent_rx,
            status: crate::torrent::TorrentStatus::Downloading,
        };

        disk.new_peer(peer_ctx.clone());
        disk.new_torrent(torrent_ctx).await?;
        // daemon.new_torrent(magnet).await?;

        spawn(async move {
            disk.run().await?;
            Ok::<(), Error>(())
        });

        spawn(async move {
            daemon.run().await?;
            Ok::<(), Error>(())
        });

        spawn(async move {
            torrent.run().await?;
            Ok::<(), Error>(())
        });

        spawn(async move {
            let (_socket, _) = listener.accept().await.unwrap();
            let socket = Framed::new(stream, CoreCodec);
            let (sink, stream) = socket.split();

            let mut peer = Peer::<peer::Connected> {
                state: peer::Connected {
                    pieces,
                    ctx: peer_ctx_,
                    ext_states: peer::ExtStates::default(),
                    sink,
                    stream,
                    incoming_requests: Vec::new(),
                    outgoing_requests: Vec::new(),
                    outgoing_requests_info_pieces: Vec::new(),
                    session: peer::session::Session::default(),
                    have_info: true,
                    reserved: bitvec::bitarr![u8, bitvec::prelude::Msb0; 0; 8 * 8],
                    torrent_ctx: torrent_ctx_,
                    rx: peer_rx,
                },
            };

            peer.run().await.unwrap();
        });

        let (otx, orx) = oneshot::channel();
        disk_tx
            .send(DiskMsg::RequestBlocks {
                info_hash,
                peer_id: peer_ctx.id.clone(),
                recipient: otx,
                qnt: 3,
            })
            .await?;
        let blocks = orx.await?;
        println!("blocks {blocks:#?}");

        assert_eq!(
            blocks,
            vec![
                BlockInfo { index: 0, begin: 0, len: BLOCK_LEN },
                BlockInfo { index: 1, begin: 0, len: BLOCK_LEN },
                BlockInfo { index: 3, begin: 0, len: BLOCK_LEN },
            ]
        );

        let _ = tokio::fs::remove_dir_all(format!("/tmp/{download_dir}")).await;

        assert!(false);

        Ok(())
    }
}
