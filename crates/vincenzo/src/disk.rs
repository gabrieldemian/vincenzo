//! Disk is responsible for file I/O of all Torrents.
use std::{
    collections::VecDeque,
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
    metainfo,
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
        recipient: Sender<VecDeque<BlockInfo>>,
        qnt: usize,
    },
    /// When a peer is Choked, or receives an error and must close the
    /// connection, the outgoing/pending blocks of this peer must be
    /// appended back to the list of available block_infos.
    ReturnBlockInfos(InfoHash, VecDeque<BlockInfo>),
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

// A metainfo file, but the length is accumulated.
#[derive(Debug, Clone, Default, PartialEq)]
struct DiskFile {
    path: Vec<String>,
    length: u64,
}

// A cache of the Info of a torrent,
// used to avoid the cost of doing read locks all the time.
#[derive(Debug, Clone)]
struct TorrentInfo {
    name: String,
    total_size: u64,
    piece_length: u32,
    pieces: u32,
    files: Vec<DiskFile>,
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

    /// How many bytes downloaded for each piece.
    pub downloaded_pieces: HashMap<InfoHash, Vec<u64>>,
    pub piece_strategy: HashMap<InfoHash, PieceStrategy>,
    pub download_dir: String,
    cache: HashMap<InfoHash, Vec<Vec<Block>>>,
    torrent_info: HashMap<InfoHash, TorrentInfo>,

    /// The block infos of each piece of a torrent, ordered from 0 to last.
    /// where the index of the VecDeque is a piece.
    pieces_blocks: HashMap<InfoHash, Vec<VecDeque<BlockInfo>>>,
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
            downloaded_pieces: HashMap::new(),
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
                    info!("new_torrent");
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
                    println!("RequestBlocks");
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
                    debug!("NewPeer");
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
                            .get_mut(block.index as usize)
                        {
                            piece.push_back(block);
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
        root_dir: &str,
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

        self.torrent_ctxs.insert(info_hash.clone(), torrent_ctx.clone());

        let torrent_ctx = self.torrent_ctxs.get(info_hash).unwrap();
        let info = torrent_ctx.info.read().await;

        let mut disk_files = Vec::new();

        // create a cache of the info to avoid
        // calling a read lock everytime.
        if let Some(files) = &info.files {
            let mut counter = 0_u64;
            debug!("info.files {files:?}");

            for f in files {
                disk_files
                    .push(DiskFile { path: f.path.clone(), length: counter });
                counter += f.length as u64;
            }
        } else {
            disk_files.push(DiskFile {
                path: vec![info.name.clone()],
                length: info.file_length.unwrap() as u64,
            });
        }

        self.torrent_info.insert(
            info_hash.clone(),
            TorrentInfo {
                name: info.name.clone(),
                total_size: info.get_size(),
                piece_length: info.piece_length,
                pieces: info.pieces(),
                files: disk_files,
            },
        );

        let base = self.base_path(info_hash);

        // create "skeleton" of the torrent, empty files and directories
        if let Some(files) = info.files.clone() {
            for mut file in files {
                // extract the file, the last item of the vec
                // file.txt
                let last = file.path.pop();

                // the directory of the current file
                // download_dir/name_of_torrent/name_of_dir
                let mut file_dir = base.clone();
                for dir_path in file.path {
                    file_dir.push(&dir_path);
                }

                create_dir_all(&file_dir).await?;

                // now with the dirs created, we create the file
                if let Some(file_ext) = last {
                    file_dir.push(file_ext);
                    Self::open_file(file_dir).await?;
                }
            }
        }

        let pieces_len = info.pieces();

        self.piece_strategy.insert(info_hash.clone(), PieceStrategy::default());
        let piece_order = self.piece_strategy.get(info_hash).cloned().unwrap();

        let mut r: Vec<u32> = (0..pieces_len).collect();
        let downloaded_pieces = vec![0; pieces_len as usize];

        if piece_order != PieceStrategy::Sequential {
            r.shuffle(&mut rand::rng());
        }

        info!("shuffled pieces {r:?}");

        // each index of Vec is a piece index, that is a VecDeque of blocks
        let mut pieces_blocks: Vec<VecDeque<BlockInfo>> =
            Vec::with_capacity(r.len());

        debug!("self.pieces {:?}", r);
        self.pieces.insert(info_hash.clone(), r);

        let cache_vec = vec![Vec::new(); pieces_len as usize];
        self.cache.insert(info_hash.clone(), cache_vec);

        self.downloaded_pieces.insert(info_hash.clone(), downloaded_pieces);

        // generate all block_infos of this torrent
        for block in info.get_block_infos()? {
            // and place each block_info into it's corresponding
            // piece, which is the index of `pieces_blocks`
            match pieces_blocks.get_mut(block.index as usize) {
                None => {
                    pieces_blocks.push(vec![block].into());
                }
                Some(g) => {
                    g.push_back(block);
                }
            };
        }
        self.pieces_blocks.insert(info_hash.clone(), pieces_blocks);
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

    /// The function will get the next available piece
    /// based on the following criteria:
    ///
    /// - it will respect `PieceStrategy`.
    /// - the peer must have the piece (the bit is set to 1 on it's bitfield).
    /// - the local peer (client) doesn't have the piece downloaded.
    ///
    /// # Return
    /// if `Disk` does not have the peer_ctx of the given peer_id, it will
    /// return None.
    async fn next_piece(
        &self,
        info_hash: &InfoHash,
        peer_id: &PeerId,
    ) -> Option<(usize, u32)> {
        let peer_ctx = self.peer_ctxs.iter().find(|v| v.id == *peer_id)?;

        let (otx, orx) = oneshot::channel();
        let _ = peer_ctx.tx.send(PeerMsg::GetPieces(otx)).await;

        let peer_pieces = orx.await.ok()?;
        let downloaded_pieces = self.downloaded_pieces.get(info_hash).unwrap();

        self.pieces
            .get(info_hash)
            .unwrap()
            .iter()
            .enumerate()
            .find(|(_, piece)| {
                if let Some(has_piece) = peer_pieces.get(**piece as usize) {
                    if *has_piece
                        && *downloaded_pieces.get(**piece as usize).unwrap()
                            < self.piece_size(info_hash, **piece as usize)
                                as u64
                    {
                        return true;
                    }
                }
                false
            })
            .map(|(i, x)| (i, x.to_owned()))
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

    /// Request blocks of the given `peer_id` that has not been requested
    /// nor downloaded yet, given a `qnt` quantity.
    #[tracing::instrument(skip_all)]
    pub async fn request_blocks(
        &mut self,
        info_hash: &InfoHash,
        peer_id: &PeerId,
        qnt: usize,
    ) -> Result<VecDeque<BlockInfo>, Error> {
        let mut result: VecDeque<BlockInfo> = VecDeque::new();

        for _ in 0..qnt {
            let next_piece = self.next_piece(info_hash, peer_id).await;

            if let Some(piece) = next_piece {
                let pieces_blocks =
                    self.pieces_blocks.get_mut(info_hash).unwrap();

                let blocks = pieces_blocks.get_mut(piece.1 as usize);

                if let Some(blocks) = blocks {
                    if blocks.is_empty() {
                        debug!(
                            "piece {} is empty, removing by index {}",
                            piece.1, piece.0
                        );
                        // pieces_blocks.remove(piece.1 as usize);
                        self.pieces.get_mut(info_hash).unwrap().remove(piece.0);
                    }
                    // how many blocks are left to request
                    let left = qnt - result.len();
                    result.extend(blocks.drain(0..left.min(blocks.len())));
                }
            }

            if result.len() >= qnt {
                break;
            };
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

    /// The essence of the entire Disk struct is in this function,
    /// It will first try to write the block to the `cache`.
    ///
    /// # When a full piece is downloaded
    ///
    /// It is only after all blocks of the piece has been downloaded on `cache`,
    /// that the function will write all the bytes into disk.
    ///
    /// Whenever a full piece is downloaded, this function will call
    /// `validate_piece` to validate the full piece hash.
    ///
    /// If the download algorithm of the pieces is set to "Random", and this
    /// function has downloaded it's first full piece, it will change the
    /// algorithm to rarest-first.
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

        let torrent_tx = torrent_ctx.tx.clone();

        self.cache.get_mut(info_hash).ok_or(Error::TorrentDoesNotExist)?[index]
            .push(block);

        let _ =
            torrent_tx.send(TorrentMsg::IncrementDownloaded(len as u64)).await;

        let downloaded_piece_bytes = self
            .downloaded_pieces
            .get_mut(info_hash)
            .ok_or(Error::TorrentDoesNotExist)?
            .get_mut(index)
            .unwrap();

        *downloaded_piece_bytes += len as u64;

        // Check if the entire piece of the `block` has been downloaded
        if *downloaded_piece_bytes >= self.piece_size(info_hash, index) as u64 {
            let downloaded_pieces_len = self
                .downloaded_pieces_len
                .get_mut(info_hash)
                .ok_or(Error::TorrentDoesNotExist)?;

            *downloaded_pieces_len += 1;

            if *downloaded_pieces_len == 1 {
                let piece_order = self
                    .piece_strategy
                    .get(info_hash)
                    .ok_or(Error::TorrentDoesNotExist)?;

                if *piece_order == PieceStrategy::Random {
                    debug!(
                        "first piece downloaded, and piece order is random, \
                         switching to rarest-first"
                    );
                    self.rarest_first(info_hash).await?;
                }
            }

            // validate that the downloaded pieces hash
            // matches the hash of the info.
            let piece_validation = self.validate_piece(info_hash, index).await;

            match piece_validation {
                Ok(_) => {
                    debug!("piece {index} is valid.");

                    let _ =
                        torrent_tx.send(TorrentMsg::SetBitfield(index)).await;
                    let _ = torrent_tx
                        .send(TorrentMsg::DownloadedPiece(index))
                        .await;
                }
                Err(_) => {
                    warn!("piece {index} is corrupted.");
                }
            }

            // at this point the piece is valid,
            // get the file path of all the blocks,
            // and then write all bytes into the files.
            self.write_pieces(info_hash, index).await?;
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
        &self,
        info_hash: &InfoHash,
        index: usize,
    ) -> Result<(), Error> {
        let b = index * 20;
        let e = b + 20;

        let pieces =
            &self.torrent_ctxs.get(info_hash).unwrap().info.read().await.pieces;

        let hash_from_info = pieces[b..e].to_owned();

        let mut hash = sha1_smol::Sha1::new();

        let mut blocks: Vec<Block> =
            self.cache.get(info_hash).unwrap()[index].clone();
        blocks.sort();

        for block in &blocks {
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
        let mut blocks: Vec<Block> =
            self.cache.get_mut(info_hash).unwrap()[piece].drain(..).collect();

        blocks.sort();

        let torrent_info = self.torrent_info.get(info_hash).unwrap();
        let files = &torrent_info.files;
        let piece_length = torrent_info.piece_length as u64;
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
            let acc_length = files
                .iter()
                .find(|f| {
                    let mut path = PathBuf::new();
                    path.extend(f.path.clone());
                    path == file_path_buf
                })
                .map(|v| v.length)
                .unwrap();

            let file_offset = piece_offset.saturating_sub(acc_length);

            debug!("file_offset {file_offset}");

            file.seek(SeekFrom::Start(file_offset)).await?;
            file.write_all(&bytes).await?;
        }

        Ok(())
    }
    /// Get the correct piece size, the last piece of a torrent
    /// might be smaller than the other pieces.
    fn piece_size(&self, info_hash: &InfoHash, piece_index: usize) -> u32 {
        let v = self.torrent_info.get(info_hash).unwrap();
        if piece_index == v.pieces as usize - 1 {
            let remainder = v.total_size % v.piece_length as u64;
            if remainder == 0 {
                v.piece_length
            } else {
                remainder as u32
            }
        } else {
            v.piece_length
        }
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
        net::{Ipv4Addr, SocketAddrV4},
        path::Path,
    };

    use rand::{distr::Alphanumeric, Rng};

    use crate::{
        config::CONFIG,
        daemon::Daemon,
        extensions::core::{Block, BLOCK_LEN},
        magnet::Magnet,
        metainfo::{self, Info},
        torrent::Torrent,
    };

    use super::*;
    use tokio::{fs, spawn, sync::mpsc};

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
        let rd = root_dir.clone();

        let original_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |panic| {
            let _ = std::fs::remove_dir_all(&rd);
            original_hook(panic);
        }));

        let magnet = format!(
            "magnet:?xt=urn:btih:9999999999999999999999999999999999999999&amp;\
             dn={torrent_dir}&amp;tr=udp%3A%2F%2Ftracker.coppersurfer.tk%\
             3A6969%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.openbittorrent.com%\
             3A6969%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.bittor.pw%3A1337%"
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

        Disk::create_file_tree(&root_dir, &files).await?;

        for df in &files {
            let mut path = PathBuf::from(&root_dir);
            path.extend(&df.path);

            assert!(Disk::open_file(&path).await.is_ok());
            assert!(Path::new(&path).is_file());
        }
        // =======================
        // spawning boilerplate
        // =======================

        // let (disk_tx, _disk_rx) = mpsc::channel::<DiskMsg>(100);
        // let mut disk = Disk::new(CONFIG.download_dir.clone());
        // let disk_tx = disk.tx.clone();
        //
        // spawn(async move {
        //     let _ = disk.run().await;
        // });
        //
        // let mut daemon = Daemon::new(disk_tx);
        // daemon.run().await?;
        //
        // let daemon = Daemon::new(disk_tx.clone());
        //
        // let magnet = Magnet::new(&magnet).unwrap();
        // let torrent = Torrent::new(disk_tx, daemon.ctx.clone(), magnet);
        // let torrent_ctx = torrent.ctx.clone();
        //
        // let mut disk = Disk::new(download_dir.clone());
        //
        // let info = Info {
        //     metadata_size: None,
        //     file_length: None,
        //     name,
        //     piece_length: BLOCK_LEN,
        //     pieces: vec![],
        //     files: Some(files.clone()),
        // };

        //
        // can write files
        //

        //
        // can open files
        //

        // disk.torrent_ctxs
        //     .insert(torrent_ctx.info_hash.clone(), torrent_ctx.clone());
        //
        // let mut info_ctx = torrent.ctx.info.write().await;
        // *info_ctx = info.clone();
        // drop(info_ctx);
        //
        // disk.new_torrent(torrent_ctx.clone()).await.unwrap();
        //
        // let mut path = PathBuf::new();
        // path.push(&download_dir);
        // path.push(&info.name);
        // path.push("foo.txt");
        //
        // let result = Disk::open_file(path).await;
        // assert!(result.is_ok());
        //
        // let mut path = PathBuf::new();
        // path.push(&download_dir);
        // path.push(&info.name);
        // path.push("bar/baz.txt");
        // let result = Disk::open_file(path).await;
        // assert!(result.is_ok());
        //
        // let mut path = PathBuf::new();
        // path.push(&download_dir);
        // path.push(&info.name);
        // path.push("bar/buzz/bee.txt");
        // let result = Disk::open_file(path).await;
        // assert!(result.is_ok());
        //
        // assert!(Path::new(&format!("{download_dir}/bla/foo.txt")).is_file());
        // assert!(Path::new(&format!("{download_dir}/bla/bar/baz.txt")).
        // is_file()); assert!(Path::new(&format!("{download_dir}/bla/
        // bar/buzz/bee.txt"))     .is_file());

        tokio::fs::remove_dir_all(root_dir).await.unwrap();

        Ok(())
    }
}
