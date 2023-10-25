//! Disk is responsible for file I/O of all Torrents.
use std::{
    collections::VecDeque,
    io::SeekFrom,
    path::{Path, PathBuf},
    sync::Arc,
};

use hashbrown::HashMap;
use rand::seq::SliceRandom;
use tokio::{
    fs::{create_dir_all, File, OpenOptions},
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, BufWriter},
    sync::{mpsc::Receiver, oneshot::Sender},
};
use tracing::debug;

use crate::{
    error::Error,
    metainfo,
    peer::{PeerCtx, PeerMsg},
    tcp_wire::{Block, BlockInfo},
    torrent::{TorrentCtx, TorrentMsg},
};

#[derive(Debug)]
pub enum DiskMsg {
    /// After the client downloaded the Info from peers, this message will be sent,
    /// to create the skeleton of the torrent on disk (empty files and folders),
    /// and to add the torrent ctx.
    NewTorrent(Arc<TorrentCtx>),
    /// The Peer does not have an ID until the handshake (that is why it's an option), when that happens,
    /// this message will be sent immediately to add the peer context.
    NewPeer(Arc<PeerCtx>),
    ReadBlock {
        block_info: BlockInfo,
        recipient: Sender<Result<Vec<u8>, Error>>,
        info_hash: [u8; 20],
    },
    /// Handle a new downloaded Piece, validate that the hash all the blocks of
    /// this piece matches the hash on Info.pieces. If the hash is valid,
    /// the fn will send a Have msg to all peers that don't have this piece.
    /// and update the bitfield of the Torrent struct.
    ValidatePiece(usize, [u8; 20], Sender<Result<(), Error>>),
    OpenFile(String, Sender<File>),
    /// Write the given block to disk, the Disk struct will get the seeked file
    /// automatically.
    WriteBlock {
        block: Block,
        // recipient: Sender<Result<(), Error>>,
        info_hash: [u8; 20],
    },
    /// Request block infos that the peer has, that we do not have ir nor requested it.
    RequestBlocks {
        qnt: usize,
        recipient: Sender<VecDeque<BlockInfo>>,
        info_hash: [u8; 20],
        peer_id: [u8; 20],
    },
    /// When a peer is Choked, or receives an error and must close the connection,
    /// the outgoing/pending blocks of this peer must be appended back
    /// to the list of available block_infos.
    ReturnBlockInfos([u8; 20], VecDeque<BlockInfo>),
    Quit,
}

/// The algorithm that determines how pieces are downloaded.
/// The recommended is [Random]. But for streaming, [Sequential] is used.
///
/// The default algorithm is to use Random first until we have
/// a complete piece, after that, we switch to Rarest first.
#[derive(Clone, Copy, Hash, PartialEq, Eq, Default, Debug)]
pub enum PieceOrder {
    /// Random First, select random pieces to download
    #[default]
    Random,
    /// Rarest First, select the rarest pieces to download,
    /// and the most common to download at the end.
    Rarest,
    /// Sequential downloads, only used in streaming.
    Sequential,
}

#[derive(Debug, Clone, Default, PartialEq)]
struct PieceInfo {
    pub index: u32,
    pub cursor: u32,
}

#[derive(Debug, Clone)]
struct TorrentInfo {
    total_size: u64,
    piece_length: u32,
    pieces: u32,
}

/// The Disk struct responsabilities:
/// - Open and create files, create directories
/// - Read/Write blocks to files
/// - Store block infos of all torrents
/// - Validate hash of pieces
#[derive(Debug)]
pub struct Disk {
    /// k: info_hash
    pub torrent_ctxs: HashMap<[u8; 20], Arc<TorrentCtx>>,
    /// k: peer_id
    pub peer_ctxs: HashMap<[u8; 20], Arc<PeerCtx>>,
    /// k: info_hash
    cache: HashMap<[u8; 20], Block>,
    /// k: info_hash
    torrent_info: HashMap<[u8; 20], TorrentInfo>,
    /// The block infos of each piece of a torrent, ordered from 0 to last.
    /// where the index of the VecDeque is a piece.
    /// k: info_hash
    pieces_blocks: HashMap<[u8; 20], Vec<VecDeque<BlockInfo>>>,
    /// The sequence in which pieces will be downloaded,
    /// based on `PieceOrder`.
    /// k: info_hash
    pieces: HashMap<[u8; 20], Vec<u32>>,
    /// How many pieces were downloaded.
    /// k: info_hash
    downloaded_pieces_len: HashMap<[u8; 20], u32>,
    /// How many bytes downloaded for each piece.
    /// k: info_hash
    downloaded_pieces: HashMap<[u8; 20], Vec<u64>>,
    /// k: info_hash
    piece_order: HashMap<[u8; 20], PieceOrder>,
    download_dir: String,
    rx: Receiver<DiskMsg>,
}

impl Disk {
    pub fn new(rx: Receiver<DiskMsg>, download_dir: String) -> Self {
        Self {
            rx,
            download_dir,
            cache: HashMap::new(),
            peer_ctxs: HashMap::new(),
            torrent_ctxs: HashMap::new(),
            downloaded_pieces_len: HashMap::new(),
            downloaded_pieces: HashMap::new(),
            pieces_blocks: HashMap::default(),
            pieces: HashMap::default(),
            piece_order: HashMap::default(),
            torrent_info: HashMap::default(),
        }
    }

    #[tracing::instrument(skip(self), name = "disk::run")]
    pub async fn run(&mut self) -> Result<(), Error> {
        debug!("disk started event loop");
        while let Some(msg) = self.rx.recv().await {
            match msg {
                DiskMsg::NewTorrent(torrent) => {
                    debug!("NewTorrent");
                    let _ = self.new_torrent(torrent).await;
                }
                DiskMsg::ReadBlock {
                    block_info,
                    recipient,
                    info_hash,
                } => {
                    debug!("ReadBlock");
                    let result = self.read_block(block_info, info_hash).await;
                    let _ = recipient.send(result);
                }
                DiskMsg::WriteBlock {
                    block,
                    // recipient,
                    info_hash,
                } => {
                    debug!("WriteBlock");
                    self.write_block(block, info_hash).await?;
                    // let _ = recipient.send(self.write_block(b, info_hash).await);
                }
                DiskMsg::OpenFile(path, tx) => {
                    debug!("OpenFile");
                    let file = self.open_file(path).await?;
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
                        .request_blocks(info_hash, qnt, peer_id)
                        .await
                        .unwrap_or_default();
                    debug!("disk sending {}", infos.len());
                    let _ = recipient.send(infos);
                }
                DiskMsg::ValidatePiece(index, info_hash, tx) => {
                    debug!("ValidatePiece");
                    let r = self.validate_piece(info_hash, index).await;
                    let _ = tx.send(r);
                }
                DiskMsg::NewPeer(peer) => {
                    debug!("NewPeer");
                    self.new_peer(peer).await?;
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

    /// Should only be called after torrent has Info downloaded
    #[tracing::instrument(skip(self, torrent_ctx), name = "new_torrent")]
    pub async fn new_torrent(&mut self, torrent_ctx: Arc<TorrentCtx>) -> Result<(), Error> {
        let info_hash = torrent_ctx.info_hash;
        debug!("{info_hash:?}");

        self.torrent_ctxs.insert(info_hash, torrent_ctx);

        let torrent_ctx = self.torrent_ctxs.get(&info_hash).unwrap();
        let info = torrent_ctx.info.read().await;

        self.torrent_info.insert(
            info_hash,
            TorrentInfo {
                total_size: info.get_size(),
                piece_length: info.piece_length,
                pieces: info.pieces(),
            },
        );

        // base is download_dir/name_of_torrent
        let mut base = PathBuf::new();
        base.push(&self.download_dir);
        base.push(&info.name);

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
                    self.open_file(file_dir).await?;
                }
            }
        }

        let pieces_len = info.pieces();

        self.piece_order.insert(info_hash, PieceOrder::default());
        let piece_order = self.piece_order.get(&info_hash).cloned().unwrap();

        let mut r: Vec<u32> = (0..pieces_len).collect();
        let downloaded_pieces = vec![0; pieces_len as usize];

        if piece_order != PieceOrder::Sequential {
            r.shuffle(&mut rand::thread_rng());
        }

        // each index of Vec is a piece index, that is a VecDeque of blocks
        let mut pieces_blocks: Vec<VecDeque<BlockInfo>> = Vec::with_capacity(r.len());
        debug!("self.pieces {:?}", r);
        self.pieces.insert(info_hash, r);
        self.downloaded_pieces.insert(info_hash, downloaded_pieces);

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
        self.pieces_blocks.insert(info_hash, pieces_blocks);
        self.downloaded_pieces_len.insert(info_hash, 0);

        // tell all peers that the Info is downloaded, and
        // everything is ready to start the download.
        for peer in &self.peer_ctxs {
            peer.1.tx.send(PeerMsg::HaveInfo).await?;
        }

        Ok(())
    }

    pub async fn new_peer(&mut self, peer_ctx: Arc<PeerCtx>) -> Result<(), Error> {
        self.peer_ctxs.insert(peer_ctx.id, peer_ctx);
        Ok(())
    }

    /// This function will get the next available piece that a peer has,
    /// and that wasn't downloaded yet.
    ///
    /// It will traverse the pieces in the order of [PieceOrder].
    async fn next_piece(&self, peer_id: [u8; 20], info_hash: [u8; 20]) -> Option<(usize, u32)> {
        let peer_ctx = self.peer_ctxs.get(&peer_id);
        if peer_ctx.is_none() {
            return None;
        }
        let peer_pieces = peer_ctx.unwrap().pieces.read().await;
        let downloaded_pieces = self.downloaded_pieces.get(&info_hash).unwrap();
        self.pieces
            .get(&info_hash)
            .unwrap()
            .iter()
            .enumerate()
            .find(|(_, piece)| {
                if let Some(has_piece) = peer_pieces.get(**piece as usize) {
                    if *has_piece
                        && *downloaded_pieces.get(**piece as usize).unwrap()
                            < self.piece_size(info_hash, **piece as usize) as u64
                    {
                        return true;
                    }
                }
                return false;
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
    async fn to_rarest(&mut self, info_hash: [u8; 20]) -> Result<(), Error> {
        // get all peers of the given torrent `info_hash`
        let peer_ctxs: Vec<Arc<PeerCtx>> = self
            .peer_ctxs
            .values()
            .cloned()
            .filter(|v| v.info_hash == info_hash)
            .collect();

        debug!("calculating score of {:?} peers", peer_ctxs.len());

        if peer_ctxs.is_empty() {
            return Err(Error::NoPeers);
        }

        // pieces of the local peer
        let pieces = self
            .pieces
            .get_mut(&info_hash)
            .ok_or(Error::TorrentDoesNotExist)?;

        debug!("len of score {:?}", pieces.len());
        // vec of pieces scores/occurences, where index = piece.
        let mut score: Vec<u32> = vec![0u32; pieces.len()];

        // traverse pieces of the peers
        for ctx in peer_ctxs {
            let pieces = ctx.pieces.read().await.clone().into_iter().enumerate();
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
            let (last_idx, _) = score.iter().enumerate().last().unwrap();

            pieces.swap(last_idx, rarest_idx);

            // remove the rarest from the score,
            // as it's already used.
            score.remove(rarest_idx);
        }

        debug!("pieces changed to rarest {pieces:?}");
        let piece_order = self.piece_order.get_mut(&info_hash).unwrap();
        if *piece_order == PieceOrder::Random {
            *piece_order = PieceOrder::Rarest;
        }

        Ok(())
    }

    /// Request blocks of the given `peer_id` that has not been requested
    /// nor downloaded yet, given a `qnt` quantity.
    #[tracing::instrument(skip_all)]
    pub async fn request_blocks(
        &mut self,
        info_hash: [u8; 20],
        qnt: usize,
        peer_id: [u8; 20],
    ) -> Result<VecDeque<BlockInfo>, Error> {
        let mut result: VecDeque<BlockInfo> = VecDeque::new();

        for _ in 0..qnt {
            let next_piece = self.next_piece(peer_id, info_hash).await;

            if let Some(piece) = next_piece {
                let pieces_blocks = self.pieces_blocks.get_mut(&info_hash).unwrap();
                let blocks = pieces_blocks.get_mut(piece.1 as usize);

                if let Some(blocks) = blocks {
                    if blocks.is_empty() {
                        debug!("piece {} is empty, removing by index {}", piece.1, piece.0);
                        // pieces_blocks.remove(piece.1 as usize);
                        self.pieces.get_mut(&info_hash).unwrap().remove(piece.0);
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

    pub async fn open_file(&self, path: impl AsRef<Path>) -> Result<File, Error> {
        let path = path.as_ref().to_owned();

        OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(&path)
            .await
            .map_err(|_| Error::FileOpenError(path.to_str().unwrap().to_owned()))
    }

    pub async fn read_block(
        &self,
        block_info: BlockInfo,
        info_hash: [u8; 20],
    ) -> Result<Vec<u8>, Error> {
        let mut file = self
            .get_file_from_block_info(&block_info, info_hash)
            .await?;

        // how many bytes to read, after offset (begin)
        let mut buf = vec![0; block_info.len as usize];

        file.0.read_exact(&mut buf).await?;

        let torrent_tx = &self
            .torrent_ctxs
            .get(&info_hash)
            .ok_or(Error::TorrentDoesNotExist)?
            .tx;

        // increment uploaded count
        torrent_tx
            .send(TorrentMsg::IncrementUploaded(block_info.len))
            .await?;

        Ok(buf)
    }

    #[tracing::instrument(skip(self, block))]
    pub async fn write_block(&mut self, block: Block, info_hash: [u8; 20]) -> Result<(), Error> {
        // Write the block's data to the correct position in the file
        let len = block.block.len();
        let index = block.index;
        let block_info = BlockInfo {
            index: index as u32,
            len: len as u32,
            begin: block.begin,
        };

        // Calculate the correct position in the file and open the file
        let (mut fs_file, _) = self
            .get_file_from_block_info(&block_info, info_hash)
            .await
            .unwrap();

        // BufWriter::new(fs_file).write_all(&block.block).await?;
        fs_file.write_all(&block.block).await.unwrap();

        let torrent_ctx = self
            .torrent_ctxs
            .get(&info_hash)
            .ok_or(Error::TorrentDoesNotExist)?
            .clone();

        let torrent_tx = torrent_ctx.tx.clone();
        let _ = torrent_tx
            .send(TorrentMsg::IncrementDownloaded(len as u32))
            .await;

        let downloaded_piece_bytes = self
            .downloaded_pieces
            .get_mut(&info_hash)
            .ok_or(Error::TorrentDoesNotExist)?
            .get_mut(index)
            .unwrap();

        *downloaded_piece_bytes += len as u64;

        // Check if the entire piece of the `block` has been downloaded
        if *downloaded_piece_bytes >= self.piece_size(info_hash, index) as u64 {
            // If the entire piece has been downloaded, handle piece completion logic
            // TODO: Add validation of pieces
            // self.validate_piece(info_hash, block.index).await?;
            let _ = torrent_tx
                .send(TorrentMsg::DownloadedPiece(block.index))
                .await;

            let downloaded_pieces_len = self
                .downloaded_pieces_len
                .get_mut(&info_hash)
                .ok_or(Error::TorrentDoesNotExist)?;

            *downloaded_pieces_len += 1;

            if *downloaded_pieces_len == 1 {
                let piece_order = self
                    .piece_order
                    .get(&info_hash)
                    .ok_or(Error::TorrentDoesNotExist)?;

                if *piece_order == PieceOrder::Random {
                    debug!("first piece downloaded, and piece order is random, switching to rarest-first");
                    self.to_rarest(info_hash).await?;
                }
            }

            let mut bitfield = torrent_ctx.bitfield.write().await;
            bitfield.set(block.index, true);
        }

        Ok(())
    }

    /// Return a seeked fs::File, given an `index` and `begin`.
    /// use cases:
    /// - After we receive a Piece msg with the Block, we need to
    /// map a block to a fs::File to be able to write to disk efficiently
    /// - When a leecher sends a Request msg with a BlockInfo msg, we need
    /// to first get the corresponding file and advance the corresponding bytes
    /// of the `piece` and `begin` variables. After that, we can get the correct Block
    /// on the returned File.
    pub async fn get_file_from_block_info(
        &self,
        block_info: &BlockInfo,
        info_hash: [u8; 20],
    ) -> Result<(File, metainfo::File), Error> {
        let torrent = self
            .torrent_ctxs
            .get(&info_hash)
            .ok_or(Error::InfoHashInvalid)?;

        let info = torrent.info.read().await;

        let absolute_offset =
            block_info.index as u64 * info.piece_length as u64 + block_info.begin as u64;

        if let Some(files) = &info.files {
            let mut accumulated_length = 0u64;
            for file_info in files.iter() {
                if accumulated_length + file_info.length as u64 > absolute_offset {
                    let mut path = PathBuf::from(&self.download_dir);
                    path.push(&info.name);
                    for p in &file_info.path {
                        path.push(p);
                    }

                    let mut file = self.open_file(&path).await?;

                    let file_relative_offset = absolute_offset - accumulated_length;
                    // println!("offset {file_relative_offset}");
                    file.seek(SeekFrom::Start(file_relative_offset)).await?;

                    return Ok((file, file_info.clone()));
                }
                accumulated_length += file_info.length as u64;
            }

            return Err(Error::FileOpenError("Offset exceeds file sizes".to_owned()));
        } else {
            let mut path = PathBuf::from(&self.download_dir);
            path.push(&info.name);

            let mut file = self.open_file(path).await?;
            file.seek(SeekFrom::Start(absolute_offset)).await?;

            let file_info = metainfo::File {
                path: vec![info.name.to_owned()],
                length: info.file_length.unwrap(),
            };

            return Ok((file, file_info));
        }
    }

    pub async fn get_file_from_piece(
        &self,
        piece: u32,
        info_hash: [u8; 20],
    ) -> Result<metainfo::File, Error> {
        let torrent = self
            .torrent_ctxs
            .get(&info_hash)
            .ok_or(Error::InfoHashInvalid)?;

        let info = torrent.info.read().await;
        let piece_len = info.piece_length;

        // multi file torrent
        if let Some(files) = &info.files {
            let mut acc = 0;
            let file_info = files.iter().find(|f| {
                let pieces = f.pieces(piece_len) as u64 + acc;
                acc = pieces;
                return piece as u64 >= pieces;
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

    pub async fn get_block_from_block_info(
        &self,
        block_info: &BlockInfo,
        info_hash: [u8; 20],
    ) -> Result<Block, Error> {
        let mut file = self.get_file_from_block_info(block_info, info_hash).await?;

        let mut buf = vec![0; block_info.len as usize];

        file.0.read_exact(&mut buf).await.unwrap();

        let block = Block {
            index: block_info.index as usize,
            begin: block_info.begin,
            block: buf,
        };

        Ok(block)
    }

    #[tracing::instrument(skip(self, _info_hash))]
    pub async fn validate_piece(
        &mut self,
        _info_hash: [u8; 20],
        index: usize,
    ) -> Result<(), Error> {
        todo!();
        // let torrent_ctx = self
        //     .torrent_ctxs
        //     .get(&info_hash)
        //     .ok_or(Error::TorrentDoesNotExist)?;
        //
        // let b = index * 20;
        // let e = b + 20;
        //
        // let pieces = torrent_ctx.pieces.read().await;
        // let hash_from_info = pieces[b..e].to_owned();
        //
        // let mut buf = Vec::new();
        // let previous_blocks_len = index * (*info.blocks_per_piece() as usize);
        //
        // let downloaded_blocks = torrent_ctx
        //     .requested_blocks
        //     .read()
        //     .await
        //     .iter()
        //     .skip(previous_blocks_len)
        //     .take(5);
        //
        // for b in downloaded_blocks {
        //     buf.push(b.block)
        // }
        //
        // let mut hash = sha1_smol::Sha1::new();
        // hash.update(&buf);
        //
        // let hash = hash.digest().bytes();
        //
        // if hash_from_info != hash {
        //     return Err(Error::PieceInvalid);
        // }
    }
    fn piece_size(&self, info_hash: [u8; 20], piece_index: usize) -> u32 {
        let v = self.torrent_info.get(&info_hash).unwrap();
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
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use rand::{distributions::Alphanumeric, Rng};

    use crate::{
        bitfield::Bitfield,
        daemon::DaemonMsg,
        magnet::Magnet,
        metainfo::{self, Info},
        tcp_wire::{Block, BLOCK_LEN},
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
        let magnet = format!("magnet:?xt=urn:btih:9999999999999999999999999999999999999999&amp;dn={name}&amp;tr=udp%3A%2F%2Ftracker.coppersurfer.tk%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.openbittorrent.com%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.bittor.pw%3A1337%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.opentrackr.org%3A1337&amp;tr=udp%3A%2F%2Fbt.xxx-tracker.com%3A2710%2Fannounce&amp;tr=udp%3A%2F%2Fpublic.popcorn-tracker.org%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Feddie4.nl%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.torrent.eu.org%3A451%2Fannounce&amp;tr=udp%3A%2F%2Fp4p.arenabg.com%3A1337%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.tiny-vps.com%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Fopen.stealth.si%3A80%2Fannounce");
        let (disk_tx, disk_rx) = mpsc::channel::<DiskMsg>(1000);

        let (daemon_tx, _daemon_rx) = mpsc::channel::<DaemonMsg>(1000);

        let magnet = Magnet::new(&magnet).unwrap();
        let torrent = Torrent::new(disk_tx.clone(), daemon_tx, magnet);
        let torrent_ctx = torrent.ctx.clone();

        let mut rng = rand::thread_rng();
        let download_dir: String = (0..20).map(|_| rng.sample(Alphanumeric) as char).collect();

        let dd = download_dir.clone();
        std::panic::set_hook(Box::new(move |panic| {
            let _ = std::fs::remove_dir_all(&dd);
            original_hook(panic);
        }));

        let mut disk = Disk::new(disk_rx, download_dir.clone());

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
                    path: vec!["bar".to_owned(), "buzz".to_owned(), "bee.txt".to_owned()],
                },
            ]),
        };

        disk.torrent_ctxs
            .insert(torrent_ctx.info_hash, torrent_ctx.clone());

        let mut info_ctx = torrent.ctx.info.write().await;
        *info_ctx = info.clone();
        drop(info_ctx);

        disk.new_torrent(torrent_ctx.clone()).await.unwrap();

        let mut path = PathBuf::new();
        path.push(&download_dir);
        path.push(&info.name);
        path.push("foo.txt");

        let result = disk.open_file(path).await;
        assert!(result.is_ok());

        let mut path = PathBuf::new();
        path.push(&download_dir);
        path.push(&info.name);
        path.push("bar/baz.txt");
        let result = disk.open_file(path).await;
        assert!(result.is_ok());

        let mut path = PathBuf::new();
        path.push(&download_dir);
        path.push(&info.name);
        path.push("bar/buzz/bee.txt");
        let result = disk.open_file(path).await;
        assert!(result.is_ok());

        assert!(Path::new(&format!("{download_dir}/bla/foo.txt")).is_file());
        assert!(Path::new(&format!("{download_dir}/bla/bar/baz.txt")).is_file());
        assert!(Path::new(&format!("{download_dir}/bla/bar/buzz/bee.txt")).is_file());

        tokio::fs::remove_dir_all(download_dir).await.unwrap();
    }

    #[tokio::test]
    async fn get_file_from_block_info() {
        //
        // Complex multi file torrent, 64 blocks per piece
        //
        let original_hook = std::panic::take_hook();
        let mut rng = rand::thread_rng();
        let download_dir: String = (0..20).map(|_| rng.sample(Alphanumeric) as char).collect();
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
                metainfo::File {
                    length: 46,
                    path: vec![file_c.to_owned()],
                },
            ]),
        };

        let magnet = format!("magnet:?xt=urn:btih:9999999999999999999999999999999999999999&amp;dn={name}&amp;tr=udp%3A%2F%2Ftracker.coppersurfer.tk%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.openbittorrent.com%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.bittor.pw%3A1337%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.opentrackr.org%3A1337&amp;tr=udp%3A%2F%2Fbt.xxx-tracker.com%3A2710%2Fannounce&amp;tr=udp%3A%2F%2Fpublic.popcorn-tracker.org%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Feddie4.nl%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.torrent.eu.org%3A451%2Fannounce&amp;tr=udp%3A%2F%2Fp4p.arenabg.com%3A1337%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.tiny-vps.com%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Fopen.stealth.si%3A80%2Fannounce");

        let (disk_tx, disk_rx) = mpsc::channel::<DiskMsg>(3);

        let (fr_tx, _) = mpsc::channel::<DaemonMsg>(300);
        let magnet = Magnet::new(&magnet).unwrap();
        let torrent = Torrent::new(disk_tx.clone(), fr_tx, magnet);
        let torrent_ctx = torrent.ctx.clone();
        let info_hash: [u8; 20] = torrent_ctx.info_hash;
        let mut disk = Disk::new(disk_rx, download_dir.clone());

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

            fs_file
                .write_all(&vec![0_u8; file.length as usize])
                .await
                .unwrap();
        }

        let (_, meta_file) = disk
            .get_file_from_block_info(
                &BlockInfo {
                    index: 0,
                    begin: 0,
                    len: BLOCK_LEN,
                },
                info_hash,
            )
            .await
            .unwrap();

        assert_eq!(meta_file, info.files.as_ref().unwrap()[0]);

        // first block of the last piece
        let block = BlockInfo {
            index: 25,
            begin: 0,
            len: 16384,
        };

        let (_, meta_file) = disk
            .get_file_from_block_info(&block, info_hash)
            .await
            .unwrap();

        assert_eq!(meta_file, info.files.as_ref().unwrap()[0]);

        // last block of the first file
        let last_block = BlockInfo {
            index: 25,
            begin: 163840,
            len: 5920,
        };

        let (_, meta_file) = disk
            .get_file_from_block_info(&last_block, info_hash)
            .await
            .unwrap();

        assert_eq!(meta_file, info.files.as_ref().unwrap()[0]);

        // first block of second file
        let block = BlockInfo {
            index: 25,
            begin: 169760,
            len: BLOCK_LEN,
        };

        let (_, meta_file) = disk
            .get_file_from_block_info(&block, info_hash)
            .await
            .unwrap();

        assert_eq!(meta_file, info.files.as_ref().unwrap()[1]);

        // second block of second file
        let block = BlockInfo {
            index: 25,
            begin: 169760 + BLOCK_LEN,
            len: BLOCK_LEN,
        };

        let (_, meta_file) = disk
            .get_file_from_block_info(&block, info_hash)
            .await
            .unwrap();

        assert_eq!(meta_file, info.files.as_ref().unwrap()[1]);

        // last of second file
        let block = BlockInfo {
            index: 33,
            begin: 49152,
            len: 13625,
        };

        let (_, meta_file) = disk
            .get_file_from_block_info(&block, info_hash)
            .await
            .unwrap();

        assert_eq!(meta_file, info.files.as_ref().unwrap()[1]);

        // last file of torrent
        let block = BlockInfo {
            index: 33,
            begin: 62777,
            len: 46,
        };

        let (_, meta_file) = disk
            .get_file_from_block_info(&block, info_hash)
            .await
            .unwrap();

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
            pieces: vec![0u8; 40],
            files: Some(vec![
                metainfo::File {
                    length: 3,
                    path: vec!["out.txt".to_owned()],
                },
                metainfo::File {
                    length: 3,
                    path: vec!["last.txt".to_owned()],
                },
            ]),
        };

        let magnet = format!("magnet:?xt=urn:btih:9999999999999999999999999999999999999999&amp;dn={name}&amp;tr=udp%3A%2F%2Ftracker.coppersurfer.tk%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.openbittorrent.com%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.bittor.pw%3A1337%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.opentrackr.org%3A1337&amp;tr=udp%3A%2F%2Fbt.xxx-tracker.com%3A2710%2Fannounce&amp;tr=udp%3A%2F%2Fpublic.popcorn-tracker.org%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Feddie4.nl%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.torrent.eu.org%3A451%2Fannounce&amp;tr=udp%3A%2F%2Fp4p.arenabg.com%3A1337%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.tiny-vps.com%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Fopen.stealth.si%3A80%2Fannounce");
        let mut rng = rand::thread_rng();
        let download_dir: String = (0..20).map(|_| rng.sample(Alphanumeric) as char).collect();

        let dd = download_dir.clone();
        std::panic::set_hook(Box::new(move |panic| {
            let _ = std::fs::remove_dir_all(&dd);
            original_hook(panic);
        }));

        let (disk_tx, _) = mpsc::channel::<DiskMsg>(3);

        let (_, rx) = mpsc::channel(5);
        let mut disk = Disk::new(rx, download_dir.clone());

        let (fr_tx, _) = mpsc::channel::<DaemonMsg>(300);
        let magnet = Magnet::new(&magnet).unwrap();
        let torrent = Torrent::new(disk_tx, fr_tx, magnet);
        let info_hash = torrent.ctx.info_hash;

        let mut info_t = torrent.ctx.info.write().await;
        *info_t = info.clone();
        drop(info_t);

        disk.new_torrent(torrent.ctx.clone()).await.unwrap();

        *disk.piece_order.get_mut(&info_hash).unwrap() = PieceOrder::Sequential;

        let mut p = torrent.ctx.bitfield.write().await;
        *p = Bitfield::from_vec(vec![255]);
        drop(info);
        drop(p);

        //
        //  WRITE
        //

        // second file
        let block = Block {
            index: 1,
            begin: 2,
            block: "9".as_bytes().to_owned(),
        };
        disk.write_block(block, info_hash).await.unwrap();
        let block = Block {
            index: 1,
            begin: 1,
            block: "w".as_bytes().to_owned(),
        };
        disk.write_block(block, info_hash).await.unwrap();
        let block = Block {
            index: 1,
            begin: 0,
            block: "x".as_bytes().to_owned(),
        };
        disk.write_block(block, info_hash).await.unwrap();

        // first file
        let block = Block {
            index: 0,
            begin: 2,
            block: "3".as_bytes().to_owned(),
        };
        disk.write_block(block, info_hash).await.unwrap();
        let block = Block {
            index: 0,
            begin: 1,
            block: "1".as_bytes().to_owned(),
        };
        disk.write_block(block, info_hash).await.unwrap();
        let block = Block {
            index: 0,
            begin: 0,
            block: "2".as_bytes().to_owned(),
        };
        disk.write_block(block, info_hash).await.unwrap();

        let mut d = disk
            .open_file(format!("{download_dir}/arch/out.txt"))
            .await
            .unwrap();

        let mut buf = String::new();
        d.read_to_string(&mut buf).await.unwrap();
        // println!("buf out.txt {buf:?}");

        assert_eq!(buf, "213");

        let mut d = disk
            .open_file(format!("{download_dir}/arch/last.txt"))
            .await
            .unwrap();

        let mut buf = Vec::new();
        d.read_to_end(&mut buf).await.unwrap();
        // println!("buf last.txt {buf:?}");

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
            piece_length: 6,
            pieces: vec![0u8; 120],
            files: Some(vec![
                metainfo::File {
                    length: 12,
                    path: vec!["foo.txt".to_owned()],
                },
                metainfo::File {
                    length: 12,
                    path: vec!["bar".to_owned(), "baz.txt".to_owned()],
                },
                metainfo::File {
                    length: 12,
                    path: vec!["bar".to_owned(), "buzz".to_owned(), "bee.txt".to_owned()],
                },
            ]),
        };

        let magnet = format!("magnet:?xt=urn:btih:9999999999999999999999999999999999999999&amp;dn={name}&amp;tr=udp%3A%2F%2Ftracker.coppersurfer.tk%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.openbittorrent.com%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.bittor.pw%3A1337%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.opentrackr.org%3A1337&amp;tr=udp%3A%2F%2Fbt.xxx-tracker.com%3A2710%2Fannounce&amp;tr=udp%3A%2F%2Fpublic.popcorn-tracker.org%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Feddie4.nl%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.torrent.eu.org%3A451%2Fannounce&amp;tr=udp%3A%2F%2Fp4p.arenabg.com%3A1337%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.tiny-vps.com%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Fopen.stealth.si%3A80%2Fannounce");
        let mut rng = rand::thread_rng();
        let download_dir: String = (0..20).map(|_| rng.sample(Alphanumeric) as char).collect();

        let dd = download_dir.clone();
        std::panic::set_hook(Box::new(move |panic| {
            let _ = std::fs::remove_dir_all(&dd);
            original_hook(panic);
        }));

        let (disk_tx, _) = mpsc::channel::<DiskMsg>(3);

        let (_, rx) = mpsc::channel(5);
        let mut disk = Disk::new(rx, download_dir.clone());

        let (fr_tx, _) = mpsc::channel::<DaemonMsg>(300);
        let magnet = Magnet::new(&magnet).unwrap();
        let torrent = Torrent::new(disk_tx, fr_tx, magnet);
        let mut info_t = torrent.ctx.info.write().await;
        *info_t = info.clone();
        drop(info_t);

        disk.new_torrent(torrent.ctx.clone()).await.unwrap();

        let info_hash = torrent.ctx.info_hash;

        let mut p = torrent.ctx.bitfield.write().await;
        *p = Bitfield::from_vec(vec![255, 255, 255, 255]);
        drop(info);
        drop(p);

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

        let result = disk.write_block(block.clone(), info_hash).await;
        assert!(result.is_ok());

        // validate that the first file contains the bytes that we wrote
        let block_info = BlockInfo {
            index: 0,
            begin: 0,
            len: block.block.len() as u32,
        };
        let result = disk.read_block(block_info, info_hash).await;
        assert_eq!(result.unwrap(), block.block);

        // write a block before reading it
        // write entire second file (/bar/baz.txt)
        let block = Block {
            index: 2,
            begin: 0,
            block: vec![13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24],
        };

        let result = disk.write_block(block.clone(), info_hash).await;
        assert!(result.is_ok());

        // validate that the second file contains the bytes that we wrote
        let block_info = BlockInfo {
            index: 2,
            begin: 0,
            len: 12,
        };
        let result = disk.read_block(block_info, info_hash).await;
        assert_eq!(result.unwrap(), block.block);

        // write a block before reading it
        // write entire third file (/bar/buzz/bee.txt)
        let block = Block {
            index: 4,
            begin: 0,
            block: vec![25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36],
        };

        let result = disk.write_block(block.clone(), info_hash).await;
        assert!(result.is_ok());

        // validate that the third file contains the bytes that we wrote
        let block_info = BlockInfo {
            index: 4,
            begin: 0,
            len: 12,
        };
        let result = disk.read_block(block_info, info_hash).await;
        assert_eq!(result.unwrap(), block.block);

        //
        //  READ BLOCKS with offsets
        //

        let block_info = BlockInfo {
            index: 0,
            begin: 1,
            len: 3,
        };

        // read piece 1 block from first file
        let result = disk.read_block(block_info, info_hash).await;
        assert_eq!(result.unwrap(), vec![2, 3, 4]);

        let block_info = BlockInfo {
            index: 1,
            begin: 1,
            len: 3,
        };

        // read piece 0 block from first file
        let result = disk.read_block(block_info, info_hash).await;
        assert_eq!(result.unwrap(), vec![8, 9, 10]);

        // last thre bytes of file
        let block_info = BlockInfo {
            index: 2,
            begin: 9,
            len: 3,
        };

        // read piece 2 block from second file
        let result = disk.read_block(block_info, info_hash).await;
        assert_eq!(result.unwrap(), vec![22, 23, 24]);

        let block_info = BlockInfo {
            index: 2,
            begin: 1,
            len: 6,
        };

        // read piece 2 block from second file
        let result = disk.read_block(block_info, info_hash).await;
        assert_eq!(result.unwrap(), vec![14, 15, 16, 17, 18, 19]);

        let block_info = BlockInfo {
            index: 4,
            begin: 0,
            len: 6,
        };

        // read piece 3 block from third file
        let result = disk.read_block(block_info, info_hash).await;
        assert_eq!(result.unwrap(), vec![25, 26, 27, 28, 29, 30]);

        //
        //  VALIDATE PIECES
        //

        println!("---- piece 0 ----");
        // let r = disk.validate_piece(info_hash, 0).await;
        // assert!(r.is_ok());

        println!("---- piece 1 ----");
        // let r = disk.validate_piece(info_hash, 1).await;
        // assert!(r.is_ok());

        println!("---- piece 2 ----");
        // let r = disk.validate_piece(info_hash, 2).await;
        // assert!(r.is_ok());

        println!("---- piece 20 ----");
        // let r = disk.validate_piece(info_hash, 20).await;
        // assert!(r.is_ok());

        println!("---- piece 21 ----");
        // let r = disk.validate_piece(info_hash, 21).await;
        // assert!(r.is_ok());

        println!("---- piece 22 ----");
        // let r = disk.validate_piece(info_hash, 22).await;
        // assert!(r.is_ok());

        println!("---- piece 24 ----");
        // let r = disk.validate_piece(info_hash, 24).await;
        // assert!(r.is_ok());

        println!("---- piece 25 ----");
        // let r = disk.validate_piece(info_hash, 25).await;
        // assert!(r.is_ok());

        tokio::fs::remove_dir_all(&download_dir).await.unwrap();
    }
}
