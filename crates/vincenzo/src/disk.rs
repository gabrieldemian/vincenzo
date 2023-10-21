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
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
    sync::{mpsc::Receiver, oneshot::Sender},
};
use tracing::debug;

use crate::{
    error::Error,
    metainfo::{self, Info},
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
        b: BlockInfo,
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
        b: Block,
        recipient: Sender<Result<(), Error>>,
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
    Random,
    /// Rarest First, select the rarest pieces to download,
    /// and the most common to download at the end.
    #[default]
    Rarest,
    /// Sequential downloads, only used in streaming.
    Sequential,
}

#[derive(Debug, Clone, Default, PartialEq)]
struct PieceInfo {
    pub index: u32,
    pub cursor: u32,
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
    pieces_blocks: HashMap<[u8; 20], Vec<VecDeque<BlockInfo>>>,
    pieces: HashMap<[u8; 20], Vec<u32>>,
    downloaded_pieces: HashMap<[u8; 20], u32>,
    piece_order: HashMap<[u8; 20], PieceOrder>,
    download_dir: String,
    rx: Receiver<DiskMsg>,
}

impl Disk {
    pub fn new(rx: Receiver<DiskMsg>, download_dir: String) -> Self {
        Self {
            rx,
            download_dir,
            peer_ctxs: HashMap::new(),
            torrent_ctxs: HashMap::new(),
            downloaded_pieces: HashMap::new(),
            pieces_blocks: HashMap::default(),
            pieces: HashMap::default(),
            piece_order: HashMap::default(),
        }
    }

    /// Should only be called after torrent has Info downloaded
    #[tracing::instrument(skip(self, torrent_ctx), name = "new_torrent")]
    pub async fn new_torrent(&mut self, torrent_ctx: Arc<TorrentCtx>) -> Result<(), Error> {
        let info_hash = torrent_ctx.info_hash;
        debug!("{info_hash:?}");

        self.torrent_ctxs.insert(info_hash, torrent_ctx);

        let torrent_ctx = self.torrent_ctxs.get(&info_hash).unwrap();
        let info = torrent_ctx.info.read().await;

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

        if piece_order != PieceOrder::Sequential {
            r.shuffle(&mut rand::thread_rng());
        }

        // each index of Vec is a piece index, that is a VecDeque of blocks
        let mut pieces_blocks: Vec<VecDeque<BlockInfo>> = Vec::with_capacity(r.len());
        self.pieces.insert(info_hash, r);

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
        self.downloaded_pieces.insert(info_hash, 0);

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
        self.pieces
            .get(&info_hash)
            .unwrap()
            .iter()
            .enumerate()
            .find(|(_local_piece_idx, local_piece)| {
                peer_pieces
                    .clone()
                    .into_iter()
                    .enumerate()
                    .find(|(piece_idx, piece)| *piece_idx == **local_piece as usize && *piece)
                    .is_some()
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

        if peer_ctxs.is_empty() {
            return Err(Error::NoPeers);
        }

        let len = peer_ctxs[0].pieces.read().await.len();
        // vec of pieces scores/occurences, where index = piece.
        let mut score: Vec<u32> = vec![0u32; len];

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

        // pieces of the local peer
        let pieces = self
            .pieces
            .get_mut(&info_hash)
            .ok_or(Error::TorrentDoesNotExist)?;

        while !score.is_empty() {
            // get the rarest, the piece with the least occurences
            let (rarest_idx, _) = score.iter().enumerate().min().unwrap();

            // get the most common, the piece with the most occurences
            let (common_idx, _) = score.iter().enumerate().max().unwrap();

            pieces.swap(common_idx, rarest_idx);

            // remove the rarest from the score,
            // as it's already used.
            score.remove(rarest_idx);
        }

        let piece_order = self.piece_order.get_mut(&info_hash).unwrap();
        if *piece_order == PieceOrder::Random {
            *piece_order = PieceOrder::Rarest;
        }

        Ok(())
    }

    /// Request blocks of the given `peer_id` that has not been requested
    /// nor downloaded yet, given a `qnt` quantity.
    pub async fn request_blocks(
        &mut self,
        info_hash: [u8; 20],
        mut qnt: usize,
        peer_id: [u8; 20],
    ) -> Result<VecDeque<BlockInfo>, Error> {
        let mut infos: VecDeque<BlockInfo> = VecDeque::new();

        // get the next piece that the peer has
        let piece = self.next_piece(peer_id, info_hash).await;

        if piece.is_none() {
            return Ok(VecDeque::new());
        }

        while qnt > 0 {
            let (piece_index, _piece) = piece.unwrap();

            // get pieces_blocks of the requested torrent,
            // but also draining the Vec and taking ownership.
            let pieces_blocks = self
                .pieces_blocks
                .get_mut(&info_hash)
                .unwrap()
                .get_mut(piece_index)
                .map(|x| std::mem::take(x));

            if let Some(blocks) = pieces_blocks {
                // traverse the blocks and only get the amount
                // requested by `qnt`.
                for block in &blocks {
                    if qnt == 0 {
                        break;
                    };
                    qnt -= 1;
                    infos.push_back(block.clone());
                }
                // if the piece is not fully consumed,
                // give back the remaining blocks.
                if !blocks.is_empty() {
                    let pieces_blocks = self
                        .pieces_blocks
                        .get_mut(&info_hash)
                        .unwrap()
                        .get_mut(piece_index)
                        .unwrap();

                    for block in blocks {
                        pieces_blocks.push_back(block);
                    }
                }
                // if piece is fully requested we want to
                // delete and request the next one. Delete because
                // self.pieces is only about pieces that the local peer
                // does not have downloaded.
                let pieces = self.pieces.get_mut(&info_hash).unwrap();
                pieces.remove(piece_index);

                // if the piece order is random-first (first step of rarest-first),
                // and the local peer just downloaded its first piece,
                // change it to rarest-first algorithm.
                if let Some(downloaded_pieces) = self.downloaded_pieces.get_mut(&info_hash) {
                    *downloaded_pieces += 1;

                    let piece_order = self.piece_order.get_mut(&info_hash).unwrap();
                    if *downloaded_pieces == 1 {
                        if *piece_order == PieceOrder::Random {
                            self.to_rarest(info_hash).await?;
                        }
                    }
                }
            } else {
                break;
            }
        }

        Ok(infos)
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
                    b,
                    recipient,
                    info_hash,
                } => {
                    debug!("ReadBlock");
                    let result = self.read_block(b, info_hash).await;
                    let _ = recipient.send(result);
                }
                DiskMsg::WriteBlock {
                    b,
                    recipient,
                    info_hash,
                } => {
                    debug!("WriteBlock");
                    let _ = recipient.send(self.write_block(b, info_hash).await);
                    // let _ = recipient.send(Ok(()));
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

    #[tracing::instrument(skip(self))]
    pub async fn validate_piece(&mut self, info_hash: [u8; 20], index: usize) -> Result<(), Error> {
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

        Ok(())
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
            .send(TorrentMsg::IncrementUploaded(block_info.len as u64))
            .await?;

        Ok(buf)
    }

    #[tracing::instrument(skip(self, block))]
    pub async fn write_block(&mut self, block: Block, info_hash: [u8; 20]) -> Result<(), Error> {
        let len = block.block.len() as u32;
        let begin = block.begin;
        let index = block.index;

        let block_info = BlockInfo {
            index: index as u32,
            len,
            begin,
        };

        // let torrent_downloaded_infos = self
        //     .downloaded_infos
        //     .get_mut(&info_hash)
        //     .ok_or(Error::TorrentDoesNotExist)?;
        //
        // let already_downloaded = torrent_downloaded_infos.get(&block_info).is_some();
        //
        // if already_downloaded {
        //     debug!("already downloaded, ignoring");
        //     return Ok(());
        // }
        //
        // torrent_downloaded_infos.insert(block_info.clone());

        let (mut fs_file, _) = self
            .get_file_from_block_info(&block_info, info_hash)
            .await?;

        let torrent_ctx = self
            .torrent_ctxs
            .get(&info_hash)
            .ok_or(Error::TorrentDoesNotExist)?;

        let torrent_tx = torrent_ctx.tx.clone();

        // the write operation is heavy, so we want to spawn a thread here,
        // and only increment the downloaded count after writing,
        // because otherwise the UI could say the torrent is complete
        // while not every byte is written to disk.
        // note: spawning a task to do this write was causing bugs on Windows.
        // todo: fix this
        fs_file.write_all(&block.block).await.unwrap();
        let _ = torrent_tx
            .send(TorrentMsg::IncrementDownloaded(len as u64))
            .await;

        let mut bitfield = torrent_ctx.bitfield.write().await;
        let torrent_tx = &torrent_ctx.tx;

        let info = torrent_ctx.info.read().await;

        // if this is the last block of a piece,
        // validate the hash
        if begin + len >= info.piece_length {
            // todo: add validation of pieces
            // self.validate_piece(info_hash, index).await?;

            torrent_tx.send(TorrentMsg::DownloadedPiece(index)).await?;

            // info!("hash of piece {index:?} is valid");

            // update the bitfield of the torrent
            bitfield.set(index, true);
        }

        drop(info);

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

        // find a file on a list of files,
        // given a piece_index and a piece_len
        let piece_begin = block_info.index * info.piece_length;
        let cursor = piece_begin + block_info.begin;

        // multi file torrent
        if let Some(files) = &info.files {
            let mut file_begin: u64 = 0;
            let mut file_end: u64 = 0;

            let file_info = files.iter().enumerate().find(|(_, f)| {
                file_end += f.length as u64;

                let r = (cursor as u64 + 1) <= file_end;

                if !r {
                    file_begin += file_end;
                }
                r
            });
            if file_info.is_none() {
                return Err(Error::FileOpenError("".to_owned()));
            }
            let (_, file_info) = file_info.unwrap();

            let mut path = PathBuf::new();
            path.push(&self.download_dir);
            path.push(&info.name);
            for p in &file_info.path {
                path.push(p);
            }

            let mut file = self.open_file(&path).await?;

            let is_first_file = file_begin == 0;

            let offset = if is_first_file {
                piece_begin as u64 + block_info.begin as u64
            } else {
                let a = file_end - file_info.length as u64;
                cursor as u64 - a
            };

            file.seek(SeekFrom::Start(offset)).await?;

            return Ok((file, file_info.clone()));
        }

        let mut path = PathBuf::new();
        path.push(&self.download_dir);
        path.push(&info.name);

        // single file torrent
        let mut file = self.open_file(path).await?;
        file.seek(SeekFrom::Start(
            piece_begin as u64 + block_info.begin as u64,
        ))
        .await
        .unwrap();

        let file_info = metainfo::File {
            path: vec![info.name.to_owned()],
            length: info.file_length.unwrap(),
        };

        Ok((file, file_info))
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

    fn _get_file_from_piece_info(piece: u32, info: Info) -> Result<metainfo::File, Error> {
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
        let name = "arch".to_owned();
        let magnet = format!("magnet:?xt=urn:btih:9999999999999999999999999999999999999999&amp;dn={name}&amp;tr=udp%3A%2F%2Ftracker.coppersurfer.tk%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.openbittorrent.com%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.bittor.pw%3A1337%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.opentrackr.org%3A1337&amp;tr=udp%3A%2F%2Fbt.xxx-tracker.com%3A2710%2Fannounce&amp;tr=udp%3A%2F%2Fpublic.popcorn-tracker.org%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Feddie4.nl%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.torrent.eu.org%3A451%2Fannounce&amp;tr=udp%3A%2F%2Fp4p.arenabg.com%3A1337%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.tiny-vps.com%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Fopen.stealth.si%3A80%2Fannounce");
        let (disk_tx, disk_rx) = mpsc::channel::<DiskMsg>(1000);

        let (daemon_tx, _daemon_rx) = mpsc::channel::<DaemonMsg>(1000);

        let magnet = Magnet::new(&magnet).unwrap();
        let torrent = Torrent::new(disk_tx.clone(), daemon_tx, magnet);
        let torrent_ctx = torrent.ctx.clone();

        let mut rng = rand::thread_rng();
        let download_dir: String = (0..20).map(|_| rng.sample(Alphanumeric) as char).collect();

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

        assert!(Path::new(&format!("{download_dir}/arch/foo.txt")).is_file());
        assert!(Path::new(&format!("{download_dir}/arch/bar/baz.txt")).is_file());
        assert!(Path::new(&format!("{download_dir}/arch/bar/buzz/bee.txt")).is_file());

        tokio::fs::remove_dir_all(download_dir).await.unwrap();
    }

    #[tokio::test]
    async fn get_file_from_block_info() {
        //
        // Complex multi file torrent, 64 blocks per piece
        //
        let mut rng = rand::thread_rng();
        let download_dir: String = (0..20).map(|_| rng.sample(Alphanumeric) as char).collect();
        let name = "name_of_torrent_folder".to_owned();
        let file_a = "file_a";
        let file_b = "file_b";
        let file_c = "file_c";

        let info = Info {
            name: name.clone(),
            piece_length: 1048576,
            file_length: None,
            pieces: vec![],
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

    // if we can write, read blocks, and then validate the hash of the pieces
    #[tokio::test]
    async fn read_write_blocks_and_validate_pieces() {
        let name = "arch";

        let info = Info {
            file_length: None,
            name: name.to_owned(),
            piece_length: 6,
            pieces: vec![60; 0],
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
        let r = disk.validate_piece(info_hash, 0).await;
        assert!(r.is_ok());

        println!("---- piece 1 ----");
        let r = disk.validate_piece(info_hash, 1).await;
        assert!(r.is_ok());

        println!("---- piece 2 ----");
        let r = disk.validate_piece(info_hash, 2).await;
        assert!(r.is_ok());

        println!("---- piece 20 ----");
        let r = disk.validate_piece(info_hash, 20).await;
        assert!(r.is_ok());

        println!("---- piece 21 ----");
        let r = disk.validate_piece(info_hash, 21).await;
        assert!(r.is_ok());

        println!("---- piece 22 ----");
        let r = disk.validate_piece(info_hash, 22).await;
        assert!(r.is_ok());

        println!("---- piece 24 ----");
        let r = disk.validate_piece(info_hash, 24).await;
        assert!(r.is_ok());

        println!("---- piece 25 ----");
        let r = disk.validate_piece(info_hash, 25).await;
        assert!(r.is_ok());

        tokio::fs::remove_dir_all(&download_dir).await.unwrap();
    }
}
