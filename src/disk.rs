use std::{
    collections::{HashMap, VecDeque},
    io::SeekFrom,
    sync::Arc,
};

use tokio::{
    fs::{create_dir_all, File, OpenOptions},
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
    sync::{mpsc::Receiver, oneshot::Sender},
};

use crate::{
    error::Error,
    metainfo,
    peer::PeerCtx,
    tcp_wire::lib::{Block, BlockInfo},
    torrent::TorrentCtx,
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
    ValidatePiece(usize, Sender<Result<(), Error>>),
    OpenFile(String, Sender<File>, String),
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
}

/// The Disk struct responsabilities:
/// - Open and create files, create directories
/// - Write blocks to files given a block
/// - Read data of files given a block info
/// - Validate hash of pieces
#[derive(Debug)]
pub struct Disk {
    rx: Receiver<DiskMsg>,
    pub torrent_ctxs: HashMap<[u8; 20], Arc<TorrentCtx>>,
    pub peer_ctxs: HashMap<[u8; 20], Arc<PeerCtx>>,
}

impl Disk {
    pub fn new(rx: Receiver<DiskMsg>) -> Self {
        Self {
            rx,
            peer_ctxs: HashMap::new(),
            torrent_ctxs: HashMap::new(),
        }
    }

    pub async fn new_torrent(
        &mut self,
        torrent_ctx: Arc<TorrentCtx>,
        download_dir: &str,
    ) -> Result<(), Error> {
        let info_hash = torrent_ctx.info_hash;
        self.torrent_ctxs.insert(info_hash, torrent_ctx);

        let torrent_ctx = self.torrent_ctxs.get(&info_hash).unwrap();
        let info = torrent_ctx.info.read().await;

        // create "skeleton" of the torrent, empty files and directories
        if let Some(files) = info.files.clone() {
            for mut file in files {
                // extract the file, the last item of the vec
                let last = file.path.pop();

                // now `file.path` str only has dirs
                let dir_path = file.path.join("/");

                let file_dir = format!("{:}{:}/{dir_path}", download_dir, &info.name,);

                create_dir_all(&file_dir).await?;

                // now with the dirs created, we create the file
                if let Some(file_ext) = last {
                    self.open_file(&format!("{dir_path}/{file_ext}"), download_dir, &info.name)
                        .await?;
                }
            }
        }

        // generate block_infos of the torrent
        let mut infos = torrent_ctx.block_infos.write().await;
        *infos = info.get_block_infos()?;
        drop(infos);
        drop(info);

        Ok(())
    }

    pub async fn new_peer(&mut self, peer_ctx: Arc<PeerCtx>) -> Result<(), Error> {
        let k = peer_ctx.id.read().await.ok_or(Error::PeerIdInvalid)?;
        self.peer_ctxs.insert(k, peer_ctx);
        Ok(())
    }

    pub async fn request_blocks(
        &mut self,
        info_hash: [u8; 20],
        qnt: usize,
        peer_id: [u8; 20],
    ) -> Result<VecDeque<BlockInfo>, Error> {
        let torrent_ctx = self
            .torrent_ctxs
            .get(&info_hash)
            .ok_or(Error::InfoHashInvalid)?;

        let mut infos: VecDeque<BlockInfo> = VecDeque::new();
        let mut idxs = VecDeque::new();

        let block_infos = torrent_ctx.block_infos.read().await;
        let requested = torrent_ctx.requested_blocks.read().await;

        for (i, info) in block_infos.iter().enumerate() {
            if infos.len() >= qnt {
                break;
            }

            let was_requested = requested.iter().any(|i| i == info);
            if was_requested {
                continue;
            };

            // only request blocks that the peer has
            // let peer = torrent_ctx.pieces.read().await;
            let peer = self.peer_ctxs.get(&peer_id).ok_or(Error::PeerIdInvalid)?;
            let pieces = peer.pieces.read().await;
            if let Some(r) = pieces.get(info.index as usize) {
                if r.bit == 1 {
                    idxs.push_front(i);
                    infos.push_back(info.clone());
                }
            }
        }
        drop(block_infos);
        drop(requested);
        let mut block_infos = torrent_ctx.block_infos.write().await;
        // remove the requested infos from block_infos
        for i in idxs {
            block_infos.remove(i);
        }
        drop(block_infos);
        let mut requested = torrent_ctx.requested_blocks.write().await;
        for info in &infos {
            requested.push_back(info.clone());
        }
        drop(requested);
        Ok(infos)
    }

    #[tracing::instrument(skip(self, download_dir))]
    pub async fn run(&mut self, download_dir: String) -> Result<(), Error> {
        let download_dir = &download_dir;

        while let Some(msg) = self.rx.recv().await {
            match msg {
                // create the skeleton of the torrent tree,
                // empty folders and empty files
                DiskMsg::NewTorrent(torrent) => {
                    let _ = self.new_torrent(torrent, download_dir).await;
                }
                DiskMsg::ReadBlock {
                    b,
                    recipient,
                    info_hash,
                } => {
                    let result = self.read_block(b, download_dir, info_hash).await;
                    let _ = recipient.send(result);
                }
                DiskMsg::WriteBlock {
                    b,
                    recipient,
                    info_hash,
                } => {
                    let result = self.write_block(b, download_dir, info_hash).await;
                    let _ = recipient.send(result);
                }
                DiskMsg::OpenFile(path, tx, name) => {
                    let file = self.open_file(&path, download_dir, &name).await?;
                    let _ = tx.send(file);
                }
                DiskMsg::RequestBlocks {
                    qnt,
                    recipient,
                    info_hash,
                    peer_id,
                } => {
                    let infos = self.request_blocks(info_hash, qnt, peer_id).await?;
                    let _ = recipient.send(infos);
                }
                DiskMsg::ValidatePiece(index, tx) => {
                    let r = self.validate_piece(index).await;
                    let _ = tx.send(r);
                }
                DiskMsg::NewPeer(peer) => {
                    self.new_peer(peer).await?;
                }
            }
        }

        Ok(())
    }

    pub async fn open_file(
        &self,
        path: &str,
        download_dir: &str,
        name: &str,
    ) -> Result<File, Error> {
        let path = format!("{:}{:}/{path}", download_dir, name);

        OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(&path)
            .await
            .map_err(|_| Error::FileOpenError)
    }

    #[tracing::instrument(skip(self))]
    pub async fn validate_piece(&mut self, index: usize) -> Result<(), Error> {
        // let info = &self.ctx.info;
        // let b = index * 20;
        // let e = b + 20;
        //
        // let hash_from_info = info.pieces[b..e].to_owned();
        //
        // let mut buf = Vec::new();
        // let previous_blocks_len = index * info.blocks_per_piece() as usize;
        // let downloaded_blocks = self
        //     .torrent_ctx
        //     .requested_blocks
        //     .read()
        //     .await
        //     .iter()
        //     .skip(previous_blocks_len)
        //     .take(5);
        // for b in downloaded_blocks {
        //     buf.push(b.block)
        // }
        //
        // let mut hash = sha1_smol::Sha1::new();
        // hash.update(&buf);
        //
        // let hash = hash.digest().bytes();
        //
        // if hash_from_info == hash {
        //     return Ok(());
        // }
        // Err(Error::PieceInvalid)
        Ok(())
    }

    pub async fn read_block(
        &self,
        block_info: BlockInfo,
        download_dir: &str,
        info_hash: [u8; 20],
    ) -> Result<Vec<u8>, Error> {
        let mut file = self
            .get_file_from_block_info(&block_info, download_dir, info_hash)
            .await?;

        // how many bytes to read, after offset (begin)
        let mut buf = vec![0; block_info.len as usize];

        file.0.read_exact(&mut buf).await?;

        Ok(buf)
    }

    #[tracing::instrument(skip(self, block))]
    pub async fn write_block(
        &self,
        block: Block,
        download_dir: &str,
        info_hash: [u8; 20],
    ) -> Result<(), Error> {
        let len = block.block.clone();
        let (mut fs_file, _) = self
            .get_file_from_block_info(&block.into(), download_dir, info_hash)
            .await?;

        fs_file.write(&len).await?;

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
        download_dir: &str,
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

            let file_info = files.into_iter().enumerate().find(|(_, f)| {
                file_end += f.length as u64;

                let r = (cursor as u64 + 1) <= file_end;

                if !r {
                    file_begin += file_end;
                }
                r
            });
            if file_info.is_none() {
                return Err(Error::FileOpenError);
            }
            let (_, file_info) = file_info.unwrap();

            let path = file_info.path.join("/");
            let mut file = self.open_file(&path, download_dir, &info.name).await?;

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

        // single file torrent
        let mut file = self.open_file(&info.name, download_dir, &info.name).await?;
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

    pub async fn get_block_from_block_info(
        &self,
        block_info: &BlockInfo,
        download_dir: &str,
        info_hash: [u8; 20],
    ) -> Result<Block, Error> {
        let mut file = self
            .get_file_from_block_info(&block_info, download_dir, info_hash)
            .await?;

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

    use bendy::decoding::FromBencode;

    use crate::{
        bitfield::Bitfield,
        metainfo::{self, Info, MetaInfo},
        tcp_wire::lib::{Block, BLOCK_LEN},
        torrent::Torrent,
    };

    use super::*;
    use tokio::sync::mpsc;

    // when we send the msg `NewTorrent` the `Disk` must create
    // the "skeleton" of the torrent tree. Empty folders and empty files.
    #[tokio::test]
    async fn can_create_file_tree() {
        let magnet = "magnet:?xt=urn:btih:48aac768a865798307ddd4284be77644368dd2c7&dn=Kerkour%20S.%20Black%20Hat%20Rust...Rust%20programming%20language%202022&tr=udp%3A%2F%2Ftracker.coppersurfer.tk%3A6969%2Fannounce&tr=udp%3A%2F%2F9.rarbg.to%3A2920%2Fannounce&tr=udp%3A%2F%2Ftracker.opentrackr.org%3A1337&tr=udp%3A%2F%2Ftracker.internetwarriors.net%3A1337%2Fannounce&tr=udp%3A%2F%2Ftracker.leechers-paradise.org%3A6969%2Fannounce&tr=udp%3A%2F%2Ftracker.pirateparty.gr%3A6969%2Fannounce&tr=udp%3A%2F%2Ftracker.cyberia.is%3A6969%2Fannounce".to_owned();
        let download_dir = "btr/".to_owned();

        let (disk_tx, disk_rx) = mpsc::channel::<DiskMsg>(300);

        let torrent = Torrent::new(disk_tx.clone(), &magnet, &download_dir);
        let torrent_ctx = torrent.ctx.clone();

        let mut disk = Disk::new(disk_rx);

        let info = Info {
            file_length: None,
            name: "arch".to_owned(),
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
            .insert(torrent_ctx.info_hash.clone(), torrent_ctx.clone());

        let mut info_ctx = torrent.ctx.info.write().await;
        *info_ctx = info.clone();
        drop(info_ctx);

        disk.new_torrent(torrent_ctx.clone(), &download_dir)
            .await
            .unwrap();

        let path = "foo.txt";
        let result = disk.open_file(path, &download_dir, &info.name).await;
        assert!(result.is_ok());

        let path = "bar/baz.txt";
        let result = disk.open_file(path, &download_dir, &info.name).await;
        assert!(result.is_ok());

        let path = "bar/buzz/bee.txt";
        let result = disk.open_file(path, &download_dir, &info.name).await;
        assert!(result.is_ok());

        assert!(Path::new(&format!("{download_dir}arch/foo.txt")).is_file());
        assert!(Path::new(&format!("{download_dir}arch/bar/baz.txt")).is_file());
        assert!(Path::new(&format!("{download_dir}arch/bar/buzz/bee.txt")).is_file());
    }

    #[tokio::test]
    async fn get_file_from_block_info() {
        //
        // Complex multi file torrent, 64 blocks per piece
        //
        let metainfo = include_bytes!("../music.torrent");
        let metainfo = MetaInfo::from_bencode(metainfo).unwrap();
        let info = metainfo.info;

        let magnet = "magnet:?xt=urn:btih:9281EF9099967ED8413E87589EFD38F9B9E484B0&amp;dn=The%20Doors%20%20(Complete%20Studio%20Discography%20-%20MP3%20%40%20320kbps)&amp;tr=udp%3A%2F%2Ftracker.coppersurfer.tk%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.openbittorrent.com%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.bittor.pw%3A1337%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.opentrackr.org%3A1337&amp;tr=udp%3A%2F%2Fbt.xxx-tracker.com%3A2710%2Fannounce&amp;tr=udp%3A%2F%2Fpublic.popcorn-tracker.org%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Feddie4.nl%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.torrent.eu.org%3A451%2Fannounce&amp;tr=udp%3A%2F%2Fp4p.arenabg.com%3A1337%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.tiny-vps.com%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Fopen.stealth.si%3A80%2Fannounce ".to_owned();
        let download_dir = "btr/".to_owned();

        let (disk_tx, disk_rx) = mpsc::channel::<DiskMsg>(3);

        let torrent = Torrent::new(disk_tx.clone(), &magnet, &download_dir);
        let torrent_ctx = torrent.ctx.clone();
        let info_hash: [u8; 20] = torrent_ctx.info_hash;
        let mut disk = Disk::new(disk_rx);
        disk.torrent_ctxs
            .insert(torrent_ctx.info_hash.clone(), torrent_ctx.clone());

        let mut info_ctx = torrent.ctx.info.write().await;
        *info_ctx = info.clone();

        drop(info_ctx);

        let (_, meta_file) = disk
            .get_file_from_block_info(
                &BlockInfo {
                    index: 0,
                    begin: 0,
                    len: BLOCK_LEN,
                },
                &download_dir,
                info_hash,
            )
            .await
            .unwrap();

        assert_eq!(
            meta_file,
            metainfo::File {
                length: 26384160,
                path: vec![
                    "1967 - Strange Days".to_string(),
                    "The Doors - When The Music's Over.MP3".to_string(),
                ],
            }
        );

        // first block of the last piece
        let block = BlockInfo {
            index: 25,
            begin: 0,
            len: 16384,
        };

        let (_, meta_file) = disk
            .get_file_from_block_info(&block, &download_dir, info_hash)
            .await
            .unwrap();

        assert_eq!(
            meta_file,
            metainfo::File {
                length: 26384160,
                path: vec![
                    "1967 - Strange Days".to_string(),
                    "The Doors - When The Music's Over.MP3".to_string(),
                ],
            }
        );

        // when the peer sends the last block of the last piece of file[0]
        // block 1610 (0 idx)
        let last_block = BlockInfo {
            index: 25,
            begin: 163840,
            len: 5920,
        };

        // last of first file
        let (_, meta_file) = disk
            .get_file_from_block_info(&last_block, &download_dir, info_hash)
            .await
            .unwrap();

        assert_eq!(
            meta_file,
            metainfo::File {
                length: 26384160,
                path: vec![
                    "1967 - Strange Days".to_string(),
                    "The Doors - When The Music's Over.MP3".to_string(),
                ],
            }
        );

        // first block of second file
        let block = BlockInfo {
            index: 25,
            begin: 169760,
            len: BLOCK_LEN,
        };

        println!("----- i 25 -------");
        let (_, meta_file) = disk
            .get_file_from_block_info(&block, &download_dir, info_hash)
            .await
            .unwrap();

        assert_eq!(
            meta_file,
            metainfo::File {
                length: 8281625,
                path: vec![
                    "1967 - Strange Days".to_string(),
                    "The Doors - I Can't See Your Face In My Mind.MP3".to_string(),
                ],
            }
        );

        // second block of second file
        let block = BlockInfo {
            index: 25,
            begin: 169760 + BLOCK_LEN,
            len: BLOCK_LEN,
        };

        let (_, meta_file) = disk
            .get_file_from_block_info(&block, &download_dir, info_hash)
            .await
            .unwrap();

        assert_eq!(
            meta_file,
            metainfo::File {
                length: 8281625,
                path: vec![
                    "1967 - Strange Days".to_string(),
                    "The Doors - I Can't See Your Face In My Mind.MP3".to_string(),
                ],
            }
        );

        // last of second file
        let block = BlockInfo {
            index: 33,
            begin: 0,
            len: 13625,
        };

        println!("----- i 33 -------");
        let (_, meta_file) = disk
            .get_file_from_block_info(&block, &download_dir, info_hash)
            .await
            .unwrap();

        assert_eq!(
            meta_file,
            metainfo::File {
                length: 8281625,
                path: vec![
                    "1967 - Strange Days".to_string(),
                    "The Doors - I Can't See Your Face In My Mind.MP3".to_string(),
                ],
            }
        );

        // last file of torrent
        let block = BlockInfo {
            index: 823,
            begin: 126687,
            len: 46,
        };

        println!("----- i 823 -------");
        let (_, meta_file) = disk
            .get_file_from_block_info(&block, &download_dir, info_hash)
            .await
            .unwrap();

        assert_eq!(
            meta_file,
            metainfo::File {
                length: 46,
                path: vec!["Torrent downloaded from Demonoid.me.txt".to_string(),],
            }
        );
    }

    #[tokio::test]
    async fn validate_piece_simple_multi() {
        //
        // Simple multi file torrent, 1 block per piece
        //
        let magnet = "magnet:?xt=urn:btih:48aac768a865798307ddd4284be77644368dd2c7&dn=Kerkour%20S.%20Black%20Hat%20Rust...Rust%20programming%20language%202022&tr=udp%3A%2F%2Ftracker.coppersurfer.tk%3A6969%2Fannounce&tr=udp%3A%2F%2F9.rarbg.to%3A2920%2Fannounce&tr=udp%3A%2F%2Ftracker.opentrackr.org%3A1337&tr=udp%3A%2F%2Ftracker.internetwarriors.net%3A1337%2Fannounce&tr=udp%3A%2F%2Ftracker.leechers-paradise.org%3A6969%2Fannounce&tr=udp%3A%2F%2Ftracker.pirateparty.gr%3A6969%2Fannounce&tr=udp%3A%2F%2Ftracker.cyberia.is%3A6969%2Fannounce".to_owned();
        // path must end with "/"
        let download_dir = "btr/".to_owned();

        let metainfo = include_bytes!("../book.torrent");
        let metainfo = MetaInfo::from_bencode(metainfo).unwrap();
        let info = metainfo.info;

        let (disk_tx, disk_rx) = mpsc::channel::<DiskMsg>(3);
        let mut disk = Disk::new(disk_rx);

        let torrent = Torrent::new(disk_tx.clone(), &magnet, &download_dir);
        let torrent_ctx = torrent.ctx.clone();

        disk.torrent_ctxs
            .insert(torrent_ctx.info_hash.clone(), torrent_ctx.clone());

        let mut info_ctx = torrent.ctx.info.write().await;
        *info_ctx = info.clone();
        drop(info_ctx);

        println!("---- piece 0 ----");
        let r = disk.validate_piece(0).await;
        assert!(r.is_ok());

        println!("---- piece 1 ----");
        let r = disk.validate_piece(1).await;
        assert!(r.is_ok());

        println!("---- piece 2 ----");
        let r = disk.validate_piece(2).await;
        assert!(r.is_ok());

        println!("---- piece 249 ----");
        let r = disk.validate_piece(249).await;
        assert!(r.is_ok());
    }

    #[tokio::test]
    async fn validate_piece_complex_multi() {
        //
        // Complex multi file torrent, 64 block per piece
        //
        let magnet = "magnet:?xt=urn:btih:9281EF9099967ED8413E87589EFD38F9B9E484B0&amp;dn=The%20Doors%20%20(Complete%20Studio%20Discography%20-%20MP3%20%40%20320kbps)&amp;tr=udp%3A%2F%2Ftracker.coppersurfer.tk%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.openbittorrent.com%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.bittor.pw%3A1337%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.opentrackr.org%3A1337&amp;tr=udp%3A%2F%2Fbt.xxx-tracker.com%3A2710%2Fannounce&amp;tr=udp%3A%2F%2Fpublic.popcorn-tracker.org%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Feddie4.nl%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.torrent.eu.org%3A451%2Fannounce&amp;tr=udp%3A%2F%2Fp4p.arenabg.com%3A1337%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.tiny-vps.com%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Fopen.stealth.si%3A80%2Fannounce ".to_owned();
        let download_dir = "btr/".to_owned();

        let metainfo = include_bytes!("../book.torrent");
        let metainfo = MetaInfo::from_bencode(metainfo).unwrap();
        let info = metainfo.info;

        let (disk_tx, disk_rx) = mpsc::channel::<DiskMsg>(3);
        let mut disk = Disk::new(disk_rx);

        let torrent = Torrent::new(disk_tx.clone(), &magnet, &download_dir);
        let torrent_ctx = torrent.ctx.clone();

        disk.torrent_ctxs
            .insert(torrent_ctx.info_hash.clone(), torrent_ctx.clone());

        let mut info_ctx = torrent.ctx.info.write().await;
        *info_ctx = info.clone();
        drop(info_ctx);

        println!("---- piece 0 ----");
        let r = disk.validate_piece(0).await;
        assert!(r.is_ok());

        println!("---- piece 1 ----");
        let r = disk.validate_piece(1).await;
        assert!(r.is_ok());

        println!("---- piece 2 ----");
        let r = disk.validate_piece(2).await;
        assert!(r.is_ok());

        println!("---- piece 20 ----");
        let r = disk.validate_piece(20).await;
        assert!(r.is_ok());

        println!("---- piece 21 ----");
        let r = disk.validate_piece(21).await;
        assert!(r.is_ok());

        println!("---- piece 22 ----");
        let r = disk.validate_piece(22).await;
        assert!(r.is_ok());

        println!("---- piece 24 ----");
        let r = disk.validate_piece(24).await;
        assert!(r.is_ok());

        println!("---- piece 25 ----");
        let r = disk.validate_piece(25).await;
        assert!(r.is_ok());
    }

    #[tokio::test]
    async fn request_blocks_complex_multi() {
        //
        // Complex multi file torrent, 64 block per piece
        //
        let metainfo = include_bytes!("../music.torrent");
        let metainfo = MetaInfo::from_bencode(metainfo).unwrap();
        let magnet = "magnet:?xt=urn:btih:9281EF9099967ED8413E87589EFD38F9B9E484B0&amp;dn=The%20Doors%20%20(Complete%20Studio%20Discography%20-%20MP3%20%40%20320kbps)&amp;tr=udp%3A%2F%2Ftracker.coppersurfer.tk%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.openbittorrent.com%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.bittor.pw%3A1337%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.opentrackr.org%3A1337&amp;tr=udp%3A%2F%2Fbt.xxx-tracker.com%3A2710%2Fannounce&amp;tr=udp%3A%2F%2Fpublic.popcorn-tracker.org%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Feddie4.nl%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.torrent.eu.org%3A451%2Fannounce&amp;tr=udp%3A%2F%2Fp4p.arenabg.com%3A1337%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.tiny-vps.com%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Fopen.stealth.si%3A80%2Fannounce ".to_owned();
        let download_dir = "btr/".to_owned();

        let info = metainfo.info;

        let (disk_tx, disk_rx) = mpsc::channel::<DiskMsg>(3);
        let mut disk = Disk::new(disk_rx);

        let torrent = Torrent::new(disk_tx.clone(), &magnet, &download_dir);
        let mut infos = torrent.ctx.block_infos.write().await;
        *infos = info.get_block_infos().unwrap();
        drop(infos);

        let torrent_ctx = torrent.ctx.clone();

        disk.torrent_ctxs
            .insert(torrent_ctx.info_hash.clone(), torrent_ctx.clone());

        disk.new_torrent(torrent_ctx.clone(), &download_dir)
            .await
            .unwrap();

        let mut info_ctx = torrent.ctx.info.write().await;
        *info_ctx = info.clone();
        let mut p = torrent.ctx.pieces.write().await;
        *p = Bitfield::from(vec![0, 0, 0]);
        drop(p);
        drop(info_ctx);

        let info_hash = torrent_ctx.info_hash;

        let result = disk.request_blocks(info_hash, 2).await.unwrap();

        let expected = VecDeque::from([
            BlockInfo {
                index: 0,
                begin: 0,
                len: BLOCK_LEN,
            },
            BlockInfo {
                index: 0,
                begin: BLOCK_LEN,
                len: BLOCK_LEN,
            },
        ]);

        assert_eq!(result, expected);
    }

    #[tokio::test]
    async fn request_blocks_simple_multi() {
        //
        // Simple multi file torrent, 1 block per piece
        //
        let (disk_tx, _) = mpsc::channel::<DiskMsg>(3);
        let (_, rx) = mpsc::channel(5);
        let mut disk = Disk::new(rx);
        let magnet = "magnet:?xt=urn:btih:48aac768a865798307ddd4284be77644368dd2c7&dn=Kerkour%20S.%20Black%20Hat%20Rust...Rust%20programming%20language%202022&tr=udp%3A%2F%2Ftracker.coppersurfer.tk%3A6969%2Fannounce&tr=udp%3A%2F%2F9.rarbg.to%3A2920%2Fannounce&tr=udp%3A%2F%2Ftracker.opentrackr.org%3A1337&tr=udp%3A%2F%2Ftracker.internetwarriors.net%3A1337%2Fannounce&tr=udp%3A%2F%2Ftracker.leechers-paradise.org%3A6969%2Fannounce&tr=udp%3A%2F%2Ftracker.pirateparty.gr%3A6969%2Fannounce&tr=udp%3A%2F%2Ftracker.cyberia.is%3A6969%2Fannounce".to_owned();
        let download_dir = "btr/".to_owned();
        let torrent = Torrent::new(disk_tx, &magnet, &download_dir);
        disk.torrent_ctxs
            .insert(torrent.ctx.info_hash, torrent.ctx.clone());
        let metainfo = include_bytes!("../book.torrent");
        let mut info = torrent.ctx.info.write().await;
        let mut infos = torrent.ctx.block_infos.write().await;
        let mut p = torrent.ctx.pieces.write().await;
        *p = Bitfield::from(vec![255, 255, 255]);
        *info = MetaInfo::from_bencode(metainfo).unwrap().info;
        *infos = info.get_block_infos().unwrap();
        drop(info);
        drop(infos);
        drop(p);
        // let info_hash: [u8; 20] = Default::default();
        let info_hash = torrent.ctx.info_hash;

        let result = disk.request_blocks(info_hash, 1).await.unwrap();
        let expected = VecDeque::from([BlockInfo {
            index: 0,
            begin: 0,
            len: 16384,
        }]);

        assert_eq!(result, expected);

        // --------

        let result = disk.request_blocks(info_hash, 3).await.unwrap();

        let expected = VecDeque::from([
            BlockInfo {
                index: 1,
                begin: 0,
                len: 16384,
            },
            BlockInfo {
                index: 2,
                begin: 0,
                len: 16384,
            },
            BlockInfo {
                index: 3,
                begin: 0,
                len: 16384,
            },
        ]);

        assert_eq!(result, expected);
    }

    // given a `BlockInfo`, we must be to read the right file,
    // at the right offset.
    // when we seed to other peers, this is how it will work.
    // when we get the bytes, it's easy to just create a Block from a BlockInfo.
    #[tokio::test]
    async fn multi_file_write_read_block() {
        let magnet = "magnet:?xt=urn:btih:9281EF9099967ED8413E87589EFD38F9B9E484B0&amp;dn=The%20Doors%20%20(Complete%20Studio%20Discography%20-%20MP3%20%40%20320kbps)&amp;tr=udp%3A%2F%2Ftracker.coppersurfer.tk%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.openbittorrent.com%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.bittor.pw%3A1337%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.opentrackr.org%3A1337&amp;tr=udp%3A%2F%2Fbt.xxx-tracker.com%3A2710%2Fannounce&amp;tr=udp%3A%2F%2Fpublic.popcorn-tracker.org%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Feddie4.nl%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.torrent.eu.org%3A451%2Fannounce&amp;tr=udp%3A%2F%2Fp4p.arenabg.com%3A1337%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.tiny-vps.com%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Fopen.stealth.si%3A80%2Fannounce ".to_owned();
        let download_dir = "btr/".to_owned();

        let (disk_tx, _) = mpsc::channel::<DiskMsg>(3);

        let (_, rx) = mpsc::channel(5);
        let mut disk = Disk::new(rx);

        let torrent = Torrent::new(disk_tx, &magnet, &download_dir);
        disk.torrent_ctxs
            .insert(torrent.ctx.info_hash, torrent.ctx.clone());
        let mut info_t = torrent.ctx.info.write().await;
        let mut infos = torrent.ctx.block_infos.write().await;
        let info_hash = torrent.ctx.info_hash;

        let info = Info {
            file_length: None,
            name: "arch".to_owned(),
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

        *info_t = info.clone();
        let mut p = torrent.ctx.pieces.write().await;
        *p = Bitfield::from(vec![255, 255, 255, 255]);
        *infos = info_t.get_block_infos().unwrap();
        drop(info);
        drop(info_t);
        drop(infos);
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

        let result = disk
            .write_block(block.clone(), &download_dir, info_hash)
            .await;
        assert!(result.is_ok());

        // validate that the first file contains the bytes that we wrote
        let block_info = BlockInfo {
            index: 0,
            begin: 0,
            len: 12,
        };
        let result = disk.read_block(block_info, &download_dir, info_hash).await;
        assert_eq!(result.unwrap(), block.block);

        // write a block before reading it
        // write entire second file (/bar/baz.txt)
        let block = Block {
            index: 2,
            begin: 0,
            block: vec![13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24],
        };

        let result = disk
            .write_block(block.clone(), &download_dir, info_hash)
            .await;
        assert!(result.is_ok());

        // validate that the second file contains the bytes that we wrote
        let block_info = BlockInfo {
            index: 2,
            begin: 0,
            len: 12,
        };
        let result = disk.read_block(block_info, &download_dir, info_hash).await;
        assert_eq!(result.unwrap(), block.block);

        // write a block before reading it
        // write entire third file (/bar/buzz/bee.txt)
        let block = Block {
            index: 4,
            begin: 0,
            block: vec![25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36],
        };

        let result = disk
            .write_block(block.clone(), &download_dir, info_hash)
            .await;
        assert!(result.is_ok());

        // validate that the third file contains the bytes that we wrote
        let block_info = BlockInfo {
            index: 4,
            begin: 0,
            len: 12,
        };
        let result = disk.read_block(block_info, &download_dir, info_hash).await;
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
        let result = disk.read_block(block_info, &download_dir, info_hash).await;
        assert_eq!(result.unwrap(), vec![2, 3, 4]);

        let block_info = BlockInfo {
            index: 1,
            begin: 1,
            len: 3,
        };

        // read piece 0 block from first file
        let result = disk.read_block(block_info, &download_dir, info_hash).await;
        assert_eq!(result.unwrap(), vec![8, 9, 10]);

        // last thre bytes of file
        let block_info = BlockInfo {
            index: 2,
            begin: 9,
            len: 3,
        };

        // read piece 2 block from second file
        let result = disk.read_block(block_info, &download_dir, info_hash).await;
        assert_eq!(result.unwrap(), vec![22, 23, 24]);

        let block_info = BlockInfo {
            index: 2,
            begin: 1,
            len: 6,
        };

        // read piece 2 block from second file
        let result = disk.read_block(block_info, &download_dir, info_hash).await;
        assert_eq!(result.unwrap(), vec![14, 15, 16, 17, 18, 19]);

        let block_info = BlockInfo {
            index: 4,
            begin: 0,
            len: 6,
        };

        // read piece 3 block from third file
        let result = disk.read_block(block_info, &download_dir, info_hash).await;
        assert_eq!(result.unwrap(), vec![25, 26, 27, 28, 29, 30]);
    }
}
