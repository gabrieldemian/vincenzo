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
        oneshot::Sender,
    },
};
use tracing::{debug, warn};

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
                    debug!("NewTorrent");
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
                    debug!("RequestBlocks");
                    let infos = self
                        .request_blocks(&info_hash, &peer_id, qnt)
                        .await
                        .unwrap_or_default();
                    debug!("disk sending {}", infos.len());
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
        debug!("new_torrent {info_hash:?}");

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

        // tell all peers that the Info is downloaded, and
        // everything is ready to start the download.
        for peer in &self.peer_ctxs {
            peer.tx.send(PeerMsg::HaveInfo).await?;
        }

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
        let Some(peer_ctx) = self.peer_ctxs.iter().find(|v| v.id == *peer_id)
        else {
            return None;
        };

        let peer_pieces = peer_ctx.pieces.read().await;
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
            let pieces =
                ctx.pieces.read().await.clone().into_iter().enumerate();
            for (i, item) in pieces {
                // increment each occurence of a piece
                if item {
                    if let Some(item) = score.get_mut(i) {
                        *item += 1;
                    }
                }
            }
        }
        debug!("pieces random {pieces:?}");
        debug!("len of pieces random {:?}", pieces.len());
        debug!("score {score:?}");

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

        debug!("pieces changed to rarest {pieces:?}");
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
        debug!("result len {:?}", result.len());

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
                    debug!("Piece {index} is valid.");

                    let _ =
                        torrent_tx.send(TorrentMsg::SetBitfield(index)).await;
                    let _ = torrent_tx
                        .send(TorrentMsg::DownloadedPiece(index))
                        .await;
                }
                Err(_) => {
                    warn!("Piece {index} is corrupted.");
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
    /// - After we receive a Piece msg, we need to
    /// map the block to a fs::File to be able to write to disk.
    ///
    /// - When a leecher sends a Request msg, we need
    /// to get the corresponding file seeked on the right offset
    /// of the block info.
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
    use std::path::Path;

    use rand::{distr::Alphanumeric, Rng};

    use crate::{
        bitfield::Bitfield,
        daemon::DaemonMsg,
        extensions::core::{Block, BLOCK_LEN},
        magnet::Magnet,
        metainfo::{self, Info},
        torrent::Torrent,
    };

    use super::*;
    use tokio::{fs, sync::mpsc};

    // when we send the msg `NewTorrent` the `Disk` must create
    // the "skeleton" of the torrent tree. Empty folders and empty files.
    #[tokio::test]
    async fn create_file_tree() {
        let original_hook = std::panic::take_hook();
        let name = "bla".to_owned();
        let magnet = format!(
            "magnet:?xt=urn:btih:9999999999999999999999999999999999999999&amp;\
             dn={name}&amp;tr=udp%3A%2F%2Ftracker.coppersurfer.tk%3A6969%\
             2Fannounce&amp;tr=udp%3A%2F%2Ftracker.openbittorrent.com%3A6969%\
             2Fannounce&amp;tr=udp%3A%2F%2Ftracker.bittor.pw%3A1337%\
             2Fannounce&amp;tr=udp%3A%2F%2Ftracker.opentrackr.org%3A1337&amp;\
             tr=udp%3A%2F%2Fbt.xxx-tracker.com%3A2710%2Fannounce&amp;tr=udp%\
             3A%2F%2Fpublic.popcorn-tracker.org%3A6969%2Fannounce&amp;tr=udp%\
             3A%2F%2Feddie4.nl%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.\
             torrent.eu.org%3A451%2Fannounce&amp;tr=udp%3A%2F%2Fp4p.arenabg.\
             com%3A1337%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.tiny-vps.com%\
             3A6969%2Fannounce&amp;tr=udp%3A%2F%2Fopen.stealth.si%3A80%\
             2Fannounce"
        );
        let (disk_tx, disk_rx) = mpsc::channel::<DiskMsg>(1000);

        let (daemon_tx, _daemon_rx) = mpsc::channel::<DaemonMsg>(1000);

        let magnet = Magnet::new(&magnet).unwrap();
        let torrent = Torrent::new(disk_tx.clone(), daemon_tx, magnet);
        let torrent_ctx = torrent.ctx.clone();

        let mut rng = rand::thread_rng();
        let download_dir: String =
            (0..20).map(|_| rng.sample(Alphanumeric) as char).collect();

        let dd = download_dir.clone();
        std::panic::set_hook(Box::new(move |panic| {
            let _ = std::fs::remove_dir_all(&dd);
            original_hook(panic);
        }));

        let mut disk = Disk::new(download_dir.clone());

        let info = Info {
            file_length: None,
            name,
            piece_length: BLOCK_LEN,
            pieces: vec![],
            files: Some(vec![
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
            ]),
        };

        disk.torrent_ctxs
            .insert(torrent_ctx.info_hash.clone(), torrent_ctx.clone());

        let mut info_ctx = torrent.ctx.info.write().await;
        *info_ctx = info.clone();
        drop(info_ctx);

        disk.new_torrent(torrent_ctx.clone()).await.unwrap();

        let mut path = PathBuf::new();
        path.push(&download_dir);
        path.push(&info.name);
        path.push("foo.txt");

        let result = Disk::open_file(path).await;
        assert!(result.is_ok());

        let mut path = PathBuf::new();
        path.push(&download_dir);
        path.push(&info.name);
        path.push("bar/baz.txt");
        let result = Disk::open_file(path).await;
        assert!(result.is_ok());

        let mut path = PathBuf::new();
        path.push(&download_dir);
        path.push(&info.name);
        path.push("bar/buzz/bee.txt");
        let result = Disk::open_file(path).await;
        assert!(result.is_ok());

        assert!(Path::new(&format!("{download_dir}/bla/foo.txt")).is_file());
        assert!(Path::new(&format!("{download_dir}/bla/bar/baz.txt")).is_file());
        assert!(Path::new(&format!("{download_dir}/bla/bar/buzz/bee.txt"))
            .is_file());

        tokio::fs::remove_dir_all(download_dir).await.unwrap();
    }

    #[tokio::test]
    async fn get_file_from_block_info() {
        //
        // Complex multi file torrent, 64 blocks per piece
        //
        let original_hook = std::panic::take_hook();
        let mut rng = rand::thread_rng();
        let download_dir: String =
            (0..20).map(|_| rng.sample(Alphanumeric) as char).collect();
        let name = "name_of_torrent_folder".to_owned();
        let file_a = "file_a";
        let file_b = "file_b";
        let file_c = "file_c";

        let dd = download_dir.clone();
        std::panic::set_hook(Box::new(move |panic| {
            let _ = std::fs::remove_dir_all(&dd);
            original_hook(panic);
        }));

        let info = Info {
            name: name.clone(),
            piece_length: 1048576,
            file_length: None,
            pieces: vec![0u8; 660],
            files: Some(vec![
                metainfo::File {
                    length: 26384160,
                    path: vec![file_a.to_owned()],
                },
                metainfo::File {
                    length: 8281625,
                    path: vec![file_b.to_owned()],
                },
                metainfo::File { length: 46, path: vec![file_c.to_owned()] },
            ]),
        };

        let magnet = format!(
            "magnet:?xt=urn:btih:9999999999999999999999999999999999999999&amp;\
             dn={name}&amp;tr=udp%3A%2F%2Ftracker.coppersurfer.tk%3A6969%\
             2Fannounce&amp;tr=udp%3A%2F%2Ftracker.openbittorrent.com%3A6969%\
             2Fannounce&amp;tr=udp%3A%2F%2Ftracker.bittor.pw%3A1337%\
             2Fannounce&amp;tr=udp%3A%2F%2Ftracker.opentrackr.org%3A1337&amp;\
             tr=udp%3A%2F%2Fbt.xxx-tracker.com%3A2710%2Fannounce&amp;tr=udp%\
             3A%2F%2Fpublic.popcorn-tracker.org%3A6969%2Fannounce&amp;tr=udp%\
             3A%2F%2Feddie4.nl%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.\
             torrent.eu.org%3A451%2Fannounce&amp;tr=udp%3A%2F%2Fp4p.arenabg.\
             com%3A1337%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.tiny-vps.com%\
             3A6969%2Fannounce&amp;tr=udp%3A%2F%2Fopen.stealth.si%3A80%\
             2Fannounce"
        );

        let (disk_tx, disk_rx) = mpsc::channel::<DiskMsg>(3);

        let (fr_tx, _) = mpsc::channel::<DaemonMsg>(300);
        let magnet = Magnet::new(&magnet).unwrap();
        let torrent = Torrent::new(disk_tx.clone(), fr_tx, magnet);
        let torrent_ctx = torrent.ctx.clone();
        let info_hash: InfoHash = torrent_ctx.info_hash.clone();
        let mut disk = Disk::new(download_dir.clone());

        let mut info_ctx = torrent.ctx.info.write().await;
        *info_ctx = info.clone();
        drop(info_ctx);

        disk.new_torrent(torrent_ctx).await.unwrap();

        // write 0s to all files with their sizes
        for file in &info.files.clone().unwrap() {
            let mut path = PathBuf::new();

            path.push(download_dir.clone());
            path.push(name.clone());
            path.push(file.path[0].clone());

            let mut fs_file = fs::File::create(path).await.unwrap();

            fs_file.write_all(&vec![0_u8; file.length as usize]).await.unwrap();
        }

        let (_, meta_file) = disk
            .get_file_from_block_info(
                &BlockInfo { index: 0, begin: 0, len: BLOCK_LEN },
                &info_hash,
            )
            .await
            .unwrap();

        assert_eq!(meta_file, info.files.as_ref().unwrap()[0]);

        // first block of the last piece
        let block = BlockInfo { index: 25, begin: 0, len: 16384 };

        let (_, meta_file) =
            disk.get_file_from_block_info(&block, &info_hash).await.unwrap();

        assert_eq!(meta_file, info.files.as_ref().unwrap()[0]);

        // last block of the first file
        let last_block = BlockInfo { index: 25, begin: 163840, len: 5920 };

        let (_, meta_file) = disk
            .get_file_from_block_info(&last_block, &info_hash)
            .await
            .unwrap();

        assert_eq!(meta_file, info.files.as_ref().unwrap()[0]);

        // first block of second file
        let block = BlockInfo { index: 25, begin: 169760, len: BLOCK_LEN };

        let (_, meta_file) =
            disk.get_file_from_block_info(&block, &info_hash).await.unwrap();

        assert_eq!(meta_file, info.files.as_ref().unwrap()[1]);

        // second block of second file
        let block =
            BlockInfo { index: 25, begin: 169760 + BLOCK_LEN, len: BLOCK_LEN };

        let (_, meta_file) =
            disk.get_file_from_block_info(&block, &info_hash).await.unwrap();

        assert_eq!(meta_file, info.files.as_ref().unwrap()[1]);

        // last of second file
        let block = BlockInfo { index: 33, begin: 49152, len: 13625 };

        let (_, meta_file) =
            disk.get_file_from_block_info(&block, &info_hash).await.unwrap();

        assert_eq!(meta_file, info.files.as_ref().unwrap()[1]);

        // last file of torrent
        let block = BlockInfo { index: 33, begin: 62777, len: 46 };

        let (_, meta_file) =
            disk.get_file_from_block_info(&block, &info_hash).await.unwrap();

        assert_eq!(meta_file, info.files.as_ref().unwrap()[2]);

        tokio::fs::remove_dir_all(download_dir).await.unwrap();
    }

    #[tokio::test]
    async fn write_out_of_order() {
        let name = "arch";
        let original_hook = std::panic::take_hook();

        let info = Info {
            file_length: None,
            name: name.to_owned(),
            piece_length: 3,
            pieces: vec![
                25, 24, 125, 201, 141, 206, 82, 250, 76, 78, 142, 5, 179, 65,
                169, 183, 122, 81, 253, 38, 138, 247, 91, 50, 219, 108, 241,
                131, 238, 114, 179, 138, 39, 171, 85, 195, 131, 111, 27, 237,
            ],
            files: Some(vec![
                metainfo::File { length: 3, path: vec!["out.txt".to_owned()] },
                metainfo::File { length: 3, path: vec!["last.txt".to_owned()] },
            ]),
        };

        let magnet = format!(
            "magnet:?xt=urn:btih:9999999999999999999999999999999999999999&amp;\
             dn={name}&amp;tr=udp%3A%2F%2Ftracker.coppersurfer.tk%3A6969%\
             2Fannounce&amp;tr=udp%3A%2F%2Ftracker.openbittorrent.com%3A6969%\
             2Fannounce&amp;tr=udp%3A%2F%2Ftracker.bittor.pw%3A1337%\
             2Fannounce&amp;tr=udp%3A%2F%2Ftracker.opentrackr.org%3A1337&amp;\
             tr=udp%3A%2F%2Fbt.xxx-tracker.com%3A2710%2Fannounce&amp;tr=udp%\
             3A%2F%2Fpublic.popcorn-tracker.org%3A6969%2Fannounce&amp;tr=udp%\
             3A%2F%2Feddie4.nl%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.\
             torrent.eu.org%3A451%2Fannounce&amp;tr=udp%3A%2F%2Fp4p.arenabg.\
             com%3A1337%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.tiny-vps.com%\
             3A6969%2Fannounce&amp;tr=udp%3A%2F%2Fopen.stealth.si%3A80%\
             2Fannounce"
        );
        let mut rng = rand::thread_rng();
        let download_dir: String =
            (0..20).map(|_| rng.sample(Alphanumeric) as char).collect();

        let dd = download_dir.clone();
        std::panic::set_hook(Box::new(move |panic| {
            let _ = std::fs::remove_dir_all(&dd);
            original_hook(panic);
        }));

        let (disk_tx, _) = mpsc::channel::<DiskMsg>(3);

        let (_, rx) = mpsc::channel(5);
        let mut disk = Disk::new(download_dir.clone());

        let (fr_tx, _) = mpsc::channel::<DaemonMsg>(300);
        let magnet = Magnet::new(&magnet).unwrap();
        let torrent = Torrent::new(disk_tx, fr_tx, magnet);
        let info_hash = torrent.ctx.info_hash.clone();

        let mut info_t = torrent.ctx.info.write().await;
        *info_t = info.clone();
        drop(info_t);

        disk.new_torrent(torrent.ctx.clone()).await.unwrap();

        *disk.piece_strategy.get_mut(&info_hash).unwrap() =
            PieceStrategy::Sequential;

        // let mut p = torrent.ctx.bitfield.write().await;
        // *p = Bitfield::from_vec(vec![255]);
        drop(info);
        // drop(p);

        //
        //  WRITE
        //

        // second file
        let block =
            Block { index: 1, begin: 2, block: "9".as_bytes().to_owned() };
        disk.write_block(&info_hash, block).await.unwrap();
        let block =
            Block { index: 1, begin: 1, block: "w".as_bytes().to_owned() };
        disk.write_block(&info_hash, block).await.unwrap();
        let block =
            Block { index: 1, begin: 0, block: "x".as_bytes().to_owned() };
        disk.write_block(&info_hash, block).await.unwrap();

        // first file
        let block =
            Block { index: 0, begin: 2, block: "3".as_bytes().to_owned() };
        disk.write_block(&info_hash, block).await.unwrap();
        let block =
            Block { index: 0, begin: 1, block: "1".as_bytes().to_owned() };
        disk.write_block(&info_hash, block).await.unwrap();
        let block =
            Block { index: 0, begin: 0, block: "2".as_bytes().to_owned() };
        disk.write_block(&info_hash, block).await.unwrap();

        let mut d = Disk::open_file(format!("{download_dir}/arch/out.txt"))
            .await
            .unwrap();

        let mut buf = Vec::new();
        d.read_to_end(&mut buf).await.unwrap();
        assert_eq!(buf, vec![50, 49, 51]);

        let mut d = Disk::open_file(format!("{download_dir}/arch/last.txt"))
            .await
            .unwrap();

        let mut buf = Vec::new();
        d.read_to_end(&mut buf).await.unwrap();

        assert_eq!(buf, vec![120, 119, 57]);
        tokio::fs::remove_dir_all(&download_dir).await.unwrap();
    }

    // if we can write, read blocks, and then validate the hash of the pieces
    #[tokio::test]
    async fn read_write_blocks_and_validate_pieces() {
        let original_hook = std::panic::take_hook();
        let name = "qwerty";

        let info = Info {
            file_length: None,
            name: name.to_owned(),
            piece_length: 12,
            pieces: vec![
                38, 217, 37, 110, 112, 21, 210, 221, 197, 150, 173, 23, 34,
                152, 198, 113, 27, 205, 25, 45, 194, 40, 51, 230, 54, 11, 105,
                175, 141, 19, 33, 54, 17, 20, 203, 34, 160, 241, 116, 6, 203,
                60, 156, 40, 208, 56, 192, 60, 224, 249, 43, 30, 49, 0, 62, 13,
                220, 56, 176, 42,
            ],
            files: Some(vec![
                metainfo::File { length: 12, path: vec!["foo.txt".to_owned()] },
                metainfo::File {
                    length: 12,
                    path: vec!["bar".to_owned(), "baz.txt".to_owned()],
                },
                metainfo::File {
                    length: 12,
                    path: vec![
                        "bar".to_owned(),
                        "buzz".to_owned(),
                        "bee.txt".to_owned(),
                    ],
                },
            ]),
        };

        let magnet = format!(
            "magnet:?xt=urn:btih:9999999999999999999999999999999999999999&amp;\
             dn={name}&amp;tr=udp%3A%2F%2Ftracker.coppersurfer.tk%3A6969%\
             2Fannounce&amp;tr=udp%3A%2F%2Ftracker.openbittorrent.com%3A6969%\
             2Fannounce&amp;tr=udp%3A%2F%2Ftracker.bittor.pw%3A1337%\
             2Fannounce&amp;tr=udp%3A%2F%2Ftracker.opentrackr.org%3A1337&amp;\
             tr=udp%3A%2F%2Fbt.xxx-tracker.com%3A2710%2Fannounce&amp;tr=udp%\
             3A%2F%2Fpublic.popcorn-tracker.org%3A6969%2Fannounce&amp;tr=udp%\
             3A%2F%2Feddie4.nl%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.\
             torrent.eu.org%3A451%2Fannounce&amp;tr=udp%3A%2F%2Fp4p.arenabg.\
             com%3A1337%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.tiny-vps.com%\
             3A6969%2Fannounce&amp;tr=udp%3A%2F%2Fopen.stealth.si%3A80%\
             2Fannounce"
        );
        let mut rng = rand::thread_rng();
        let download_dir: String =
            (0..20).map(|_| rng.sample(Alphanumeric) as char).collect();

        let dd = download_dir.clone();
        std::panic::set_hook(Box::new(move |panic| {
            let _ = std::fs::remove_dir_all(&dd);
            original_hook(panic);
        }));

        let (disk_tx, _) = mpsc::channel::<DiskMsg>(3);

        let (_, rx) = mpsc::channel(5);
        let mut disk = Disk::new(download_dir.clone());

        let (fr_tx, _) = mpsc::channel::<DaemonMsg>(300);
        let magnet = Magnet::new(&magnet).unwrap();
        let torrent = Torrent::new(disk_tx, fr_tx, magnet);
        let mut info_t = torrent.ctx.info.write().await;
        *info_t = info.clone();
        drop(info_t);

        disk.new_torrent(torrent.ctx.clone()).await.unwrap();

        let info_hash = torrent.ctx.info_hash.clone();
        *disk.piece_strategy.get_mut(&info_hash).unwrap() =
            PieceStrategy::Sequential;

        // let mut p = torrent.ctx.bitfield.write().await;
        // *p = Bitfield::from_vec(vec![255, 255, 255, 255]);
        drop(info);
        // drop(p);

        //
        //  WRITE BLOCKS
        //

        // write a block before reading it
        // write entire first file (foo.txt)
        let block = Block {
            index: 0,
            begin: 0,
            block: vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12],
        };

        let result = disk.write_block(&info_hash, block.clone()).await;
        assert!(result.is_ok());

        // validate that the first file contains the bytes that we wrote
        let block_info =
            BlockInfo { index: 0, begin: 0, len: block.block.len() as u32 };
        let result = disk.read_block(&info_hash, block_info).await;
        assert_eq!(result.unwrap(), block.block);

        // write a block before reading it
        // write entire second file (/bar/baz.txt)
        let block = Block {
            index: 1,
            begin: 0,
            block: vec![13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24],
        };

        let result = disk.write_block(&info_hash, block.clone()).await;
        assert!(result.is_ok());

        // validate that the second file contains the bytes that we wrote
        let block_info = BlockInfo { index: 1, begin: 0, len: 12 };
        let result = disk.read_block(&info_hash, block_info).await;
        assert_eq!(result.unwrap(), block.block);

        // write a block before reading it
        // write entire third file (/bar/buzz/bee.txt)
        let block = Block {
            index: 2,
            begin: 0,
            block: vec![25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36],
        };

        let result = disk.write_block(&info_hash, block.clone()).await;
        assert!(result.is_ok());

        // validate that the third file contains the bytes that we wrote
        let block_info = BlockInfo { index: 2, begin: 0, len: 12 };
        let result = disk.read_block(&info_hash, block_info).await;
        assert_eq!(result.unwrap(), block.block);

        //
        //  READ BLOCKS with offsets
        //

        let block_info = BlockInfo { index: 0, begin: 1, len: 3 };

        // read piece 1 block from first file
        let result = disk.read_block(&info_hash, block_info).await;
        assert_eq!(result.unwrap(), vec![2, 3, 4]);

        let block_info = BlockInfo { index: 0, begin: 9, len: 3 };

        // read piece 0 block from first file
        let result = disk.read_block(&info_hash, block_info).await;
        assert_eq!(result.unwrap(), vec![10, 11, 12]);

        // last three bytes of file
        let block_info = BlockInfo { index: 1, begin: 9, len: 3 };

        // read piece 2 block from second file
        let result = disk.read_block(&info_hash, block_info).await;
        assert_eq!(result.unwrap(), vec![22, 23, 24]);

        let block_info = BlockInfo { index: 1, begin: 1, len: 6 };

        // read piece 2 block from second file
        let result = disk.read_block(&info_hash, block_info).await;
        assert_eq!(result.unwrap(), vec![14, 15, 16, 17, 18, 19]);

        let block_info = BlockInfo { index: 2, begin: 0, len: 6 };

        // read piece 3 block from third file
        let result = disk.read_block(&info_hash, block_info).await;
        assert_eq!(result.unwrap(), vec![25, 26, 27, 28, 29, 30]);

        tokio::fs::remove_dir_all(&download_dir).await.unwrap();
    }

    #[tokio::test]
    async fn seek_files() {
        let original_hook = std::panic::take_hook();
        let name = "seekfiles";

        let info = Info {
            piece_length: 32768,
            pieces: vec![0; 5480], // 274 pieces * 20 bytes each
            name: "name".to_string(),
            file_length: None,
            files: Some(vec![
                // 308 blocks
                metainfo::File {
                    length: 5034059,
                    path: vec!["dir".to_string(), "file_a.pdf".to_string()],
                },
                // 1 block
                metainfo::File {
                    length: 62,
                    path: vec!["file_1.txt".to_string()],
                },
                // 1 block
                metainfo::File {
                    length: 237,
                    path: vec!["file_2.txt".to_string()],
                },
            ]),
        };

        let magnet = format!(
            "magnet:?xt=urn:btih:9999999999999999999999999999999999999999&amp;\
             dn={name}&amp;tr=udp%3A%2F%2Ftracker.coppersurfer.tk%3A6969%\
             2Fannounce&amp;tr=udp%3A%2F%2Ftracker.openbittorrent.com%3A6969%\
             2Fannounce&amp;tr=udp%3A%2F%2Ftracker.bittor.pw%3A1337%\
             2Fannounce&amp;tr=udp%3A%2F%2Ftracker.opentrackr.org%3A1337&amp;\
             tr=udp%3A%2F%2Fbt.xxx-tracker.com%3A2710%2Fannounce&amp;tr=udp%\
             3A%2F%2Fpublic.popcorn-tracker.org%3A6969%2Fannounce&amp;tr=udp%\
             3A%2F%2Feddie4.nl%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.\
             torrent.eu.org%3A451%2Fannounce&amp;tr=udp%3A%2F%2Fp4p.arenabg.\
             com%3A1337%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.tiny-vps.com%\
             3A6969%2Fannounce&amp;tr=udp%3A%2F%2Fopen.stealth.si%3A80%\
             2Fannounce"
        );
        let mut rng = rand::thread_rng();
        let download_dir: String =
            (0..20).map(|_| rng.sample(Alphanumeric) as char).collect();

        let dd = download_dir.clone();
        std::panic::set_hook(Box::new(move |panic| {
            let _ = std::fs::remove_dir_all(&dd);
            original_hook(panic);
        }));

        let (disk_tx, _) = mpsc::channel::<DiskMsg>(3);

        let (_, rx) = mpsc::channel(5);
        let mut disk = Disk::new(download_dir.clone());

        let (fr_tx, _) = mpsc::channel::<DaemonMsg>(300);
        let magnet = Magnet::new(&magnet).unwrap();
        let torrent = Torrent::new(disk_tx, fr_tx, magnet);
        let mut info_t = torrent.ctx.info.write().await;
        *info_t = info.clone();
        drop(info_t);

        disk.new_torrent(torrent.ctx.clone()).await.unwrap();

        let info_hash = torrent.ctx.info_hash.clone();
        *disk.piece_strategy.get_mut(&info_hash).unwrap() =
            PieceStrategy::Sequential;

        // let mut p = torrent.ctx.bitfield.write().await;
        // *p = Bitfield::from_vec(vec![
        //     255, 255, 255, 255, 255, 255, 255, 255, 255,
        // ]);
        drop(info);
        // drop(p);

        //
        //  WRITE BLOCKS
        //

        // write a block before reading it
        let block = Block { index: 0, begin: 0, block: vec![0; 5034059] };

        let result = disk.write_block(&info_hash, block.clone()).await;
        assert!(result.is_ok());

        // write a block before reading it
        let block = Block { index: 153, begin: 20555, block: vec![0; 62] };

        let result = disk.write_block(&info_hash, block.clone()).await;
        assert!(result.is_ok());

        // let block_info = Block {
        //     index: 153,
        //     begin: 20617,
        //     block: vec![0; 237],
        // };
        let result = disk.write_block(&info_hash, block.clone()).await;
        assert!(result.is_ok());

        tokio::fs::remove_dir_all(&download_dir).await.unwrap();
    }
}
