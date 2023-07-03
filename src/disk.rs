use std::{collections::VecDeque, io::SeekFrom, sync::Arc};

use tracing::debug;
use tokio::{
    fs::{create_dir_all, File, OpenOptions},
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
    sync::{mpsc::Receiver, oneshot::Sender},
};

use crate::{
    error::Error,
    metainfo::Info,
    tcp_wire::lib::{Block, BlockInfo},
    torrent::TorrentCtx,
};

#[derive(Debug)]
pub enum DiskMsg {
    NewTorrent(Info),
    ReadBlock((BlockInfo, Sender<Result<Vec<u8>, Error>>)),
    OpenFile((String, Sender<File>)),
    WriteBlock((Block, Sender<Result<(), Error>>)),
    RequestBlocks((usize, Sender<VecDeque<BlockInfo>>)),
}

#[derive(Debug)]
pub struct Disk {
    rx: Receiver<DiskMsg>,
    pub ctx: DiskCtx,
    pub torrent_ctx: Arc<TorrentCtx>,
}

#[derive(Debug, Default)]
pub struct DiskCtx {
    pub info: Info,
    pub base_path: String,
    pub block_infos: VecDeque<BlockInfo>,
}

/// basically a wrapper for `metainfo::File`
/// includes all the block_infos of this path,
/// alongside fields from `metainfo::File`
#[derive(Debug, PartialEq, Clone, Default)]
pub struct DiskFile {
    pub length: u32,
    pub path: Vec<String>,
    // all blocks that we can request for this file
    // in sequential order, for now
    pub block_infos: VecDeque<BlockInfo>,
}

impl Disk {
    pub fn new(rx: Receiver<DiskMsg>, torrent_ctx: Arc<TorrentCtx>) -> Self {
        let ctx = DiskCtx {
            // base_path: "~/Downloads/".to_string(),
            base_path: "".to_string(),
            ..Default::default()
        };
        Self {
            rx,
            ctx,
            torrent_ctx,
        }
    }

    #[tracing::instrument]
    pub async fn run(&mut self) -> Result<(), Error> {
        debug!("running Disk event loop");

        while let Some(msg) = self.rx.recv().await {
            match msg {
                DiskMsg::NewTorrent(info) => {
                    self.ctx.block_infos = info.get_block_infos().await?;
                    self.ctx.info = info;

                    // create the skeleton of the torrent tree,
                    // empty folders and empty files
                    let mut files = self.ctx.info.files.clone();

                    if let Some(files) = &mut files {
                        for file in files {
                            // extract the file, the last item of the vec
                            let last = file.path.pop();

                            // now `file.path` str only has dirs
                            let dir_path = file.path.join("/");

                            let file_dir = format!(
                                "{:}{:}/{dir_path}",
                                &self.ctx.base_path, &self.ctx.info.name,
                            );

                            create_dir_all(&file_dir).await?;

                            // now with the dirs created, we create the file
                            if let Some(file_ext) = last {
                                self.open_file(&format!("{dir_path}/{file_ext}")).await?;
                            }
                        }
                    }

                    if let Some(_) = self.ctx.info.file_length {
                        self.open_file(&self.ctx.info.name).await?;
                    }
                }
                DiskMsg::ReadBlock((block_info, tx)) => {
                    let bytes = self.read_block(block_info).await;
                    let _ = tx.send(bytes);
                }
                DiskMsg::WriteBlock((block, tx)) => {
                    let bytes = self.write_block(block).await;
                    let _ = tx.send(bytes);
                }
                DiskMsg::OpenFile((path, tx)) => {
                    let file = self.open_file(&path).await?;
                    let _ = tx.send(file);
                }
                DiskMsg::RequestBlocks((n, tx)) => {
                    let mut requested = self.torrent_ctx.requested_blocks.write().await;
                    let mut infos: VecDeque<BlockInfo> = VecDeque::new();
                    let mut idxs = VecDeque::new();

                    for (i, info) in self.ctx.block_infos.iter_mut().enumerate() {
                        if infos.len() >= n {
                            break;
                        }

                        let was_requested = requested.iter().any(|i| i == info);
                        if was_requested {
                            continue;
                        };

                        idxs.push_front(i);
                        infos.push_back(info.clone());
                    }
                    // remove the requested items from block_infos
                    for i in idxs {
                        self.ctx.block_infos.remove(i);
                    }
                    for info in &infos {
                        requested.push_back(info.clone());
                    }

                    let _ = tx.send(infos);
                }
            }
        }

        Ok(())
    }

    pub async fn open_file(&self, path: &str) -> Result<File, Error> {
        let path = format!("{:}{:}/{path}", self.ctx.base_path, self.ctx.info.name);

        return OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(&path)
            .await
            .map_err(|_| Error::FileOpenError);
    }

    pub async fn read_block(&self, block_info: BlockInfo) -> Result<Vec<u8>, Error> {
        let mut file = self
            .get_file_from_index(block_info.index as u32, block_info.begin)
            .await?;

        // how many bytes to read, after offset (begin)
        let mut buf = vec![0; block_info.len as usize];

        file.read_exact(&mut buf).await?;

        Ok(buf)
    }

    pub async fn write_block(&self, block: Block) -> Result<(), Error> {
        let mut file = self
            .get_file_from_index(block.index as u32, block.begin)
            .await?;

        file.write_all(&block.block).await?;

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
    pub async fn get_file_from_index(&self, piece: u32, begin: u32) -> Result<File, Error> {
        // find a file on a list of files,
        // given a piece_index and a piece_len
        let piece_begin = piece * self.ctx.info.piece_length;
        let info = &self.ctx.info;

        // multi file torrent
        if let Some(files) = &info.files {
            let file = files
                .into_iter()
                .enumerate()
                .find(|(i, f)| piece_begin < f.length * (*i as u32 + 1))
                .map(|a| a.1);

            if let Some(file) = file {
                let path = file.path.join("/");

                let mut file = self.open_file(&path).await?;

                file.seek(SeekFrom::Start(piece_begin as u64 + begin as u64))
                    .await
                    .unwrap();

                return Ok(file);
            }

            return Err(Error::FileOpenError);
        }

        // single file torrent
        let mut file = self.open_file(&info.name).await?;
        file.seek(SeekFrom::Start(piece_begin as u64 + begin as u64))
            .await
            .unwrap();

        return Ok(file);
    }
    pub async fn get_block_from_block_info(&self, block_info: &BlockInfo) -> Result<Block, Error> {
        let mut file = self
            .get_file_from_index(block_info.index, block_info.begin)
            .await?;

        let mut buf = vec![0; block_info.len as usize];

        file.read_exact(&mut buf).await.unwrap();

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
    use bendy::decoding::FromBencode;

    use crate::{
        magnet_parser::get_magnet,
        metainfo::{self, Info, MetaInfo},
        tcp_wire::lib::Block,
        torrent::{Torrent, TorrentMsg},
    };

    use super::*;
    use tokio::{
        spawn,
        sync::{mpsc, oneshot},
    };

    // when we send the msg `NewTorrent` the `Disk` must create
    // the "skeleton" of the torrent tree. Empty folders and empty files.
    #[tokio::test]
    async fn create_file_tree() {
        // # examples
        // piece_len: 262144 2.6MiB
        // block_len:  16384 16KiB
        // qnt pieces: 10
        // 16 blocks in 1 piece
        //
        // # strategy
        // ask for blocks <= block_len. The last block may be smaller to fit the piece
        // |----p0----|----p1----|
        // |  b0  | b1|  b2  | b3|
        // |----------|-----------
        // |  file|0  |  |file1  |
        // -----------------------
        //    5bytes     5bytes
        //
        // b0: begin 0, len: 2
        // b1: begin: 3, len: 1
        // b2: begin: 5, len: 2
        // b2: begin: 8, len: 1
        //
        // p0: has entire b0 and half b1
        // p1: has half b1 and entire b2

        let path = "Kerkour S. Black Hat Rust...Rust programming language 2022.pdf";

        let (torrent_tx, torrent_rx) = mpsc::channel::<TorrentMsg>(1);
        let (disk_tx, _) = mpsc::channel::<DiskMsg>(1);

        let m = get_magnet("magnet:?xt=urn:btih:48aac768a865798307ddd4284be77644368dd2c7&dn=Kerkour%20S.%20Black%20Hat%20Rust...Rust%20programming%20language%202022&tr=udp%3A%2F%2Ftracker.coppersurfer.tk%3A6969%2Fannounce&tr=udp%3A%2F%2F9.rarbg.to%3A2920%2Fannounce&tr=udp%3A%2F%2Ftracker.opentrackr.org%3A1337&tr=udp%3A%2F%2Ftracker.internetwarriors.net%3A1337%2Fannounce&tr=udp%3A%2F%2Ftracker.leechers-paradise.org%3A6969%2Fannounce&tr=udp%3A%2F%2Ftracker.pirateparty.gr%3A6969%2Fannounce&tr=udp%3A%2F%2Ftracker.cyberia.is%3A6969%2Fannounce").unwrap();
        let torrent = Torrent::new(torrent_tx, disk_tx, torrent_rx, m).await;
        let torrent_ctx = torrent.ctx.clone();

        let (tx, rx) = mpsc::channel(5);
        let mut disk = Disk::new(rx, torrent_ctx);

        spawn(async move {
            disk.run().await.unwrap();
        });

        let metainfo = include_bytes!("../book.torrent");
        let metainfo = MetaInfo::from_bencode(metainfo).unwrap();
        let info = metainfo.info;

        let (tx_oneshot, rx_oneshot) = oneshot::channel();
        tx.send(DiskMsg::NewTorrent(info)).await.unwrap();
        tx.send(DiskMsg::OpenFile((path.to_owned(), tx_oneshot)))
            .await
            .unwrap();

        let file = rx_oneshot.await;

        assert!(file.is_ok());

        let metadata = file.unwrap().metadata().await.unwrap();

        assert!(!metadata.is_dir());
    }

    #[tokio::test]
    async fn request_blocks() {
        let (torrent_tx, torrent_rx) = mpsc::channel::<TorrentMsg>(3);
        let (disk_tx, _) = mpsc::channel::<DiskMsg>(3);

        let m = get_magnet("magnet:?xt=urn:btih:48aac768a865798307ddd4284be77644368dd2c7&dn=Kerkour%20S.%20Black%20Hat%20Rust...Rust%20programming%20language%202022&tr=udp%3A%2F%2Ftracker.coppersurfer.tk%3A6969%2Fannounce&tr=udp%3A%2F%2F9.rarbg.to%3A2920%2Fannounce&tr=udp%3A%2F%2Ftracker.opentrackr.org%3A1337&tr=udp%3A%2F%2Ftracker.internetwarriors.net%3A1337%2Fannounce&tr=udp%3A%2F%2Ftracker.leechers-paradise.org%3A6969%2Fannounce&tr=udp%3A%2F%2Ftracker.pirateparty.gr%3A6969%2Fannounce&tr=udp%3A%2F%2Ftracker.cyberia.is%3A6969%2Fannounce").unwrap();

        let torrent = Torrent::new(torrent_tx, disk_tx, torrent_rx, m).await;
        let torrent_ctx = torrent.ctx.clone();

        let metainfo = include_bytes!("../book.torrent");
        let metainfo = MetaInfo::from_bencode(metainfo).unwrap();
        let info = metainfo.info;

        let mut info_ctx = torrent.ctx.info.write().await;
        *info_ctx = info.clone();

        let (tx, rx) = mpsc::channel(5);
        let mut disk = Disk::new(rx, torrent_ctx);

        spawn(async move {
            disk.run().await.unwrap();
        });

        tx.send(DiskMsg::NewTorrent(info)).await.unwrap();

        let (tx_oneshot, rx_oneshot) = oneshot::channel();

        tx.send(DiskMsg::RequestBlocks((2, tx_oneshot)))
            .await
            .unwrap();

        let expected = VecDeque::from([
            BlockInfo {
                index: 0,
                begin: 0,
                len: 16384,
            },
            BlockInfo {
                index: 1,
                begin: 0,
                len: 16384,
            },
        ]);

        let result = rx_oneshot.await.unwrap();
        assert_eq!(result, expected);

        let requested = torrent.ctx.requested_blocks.read().await;
        assert_eq!(*requested, expected);
        drop(requested);

        // --------

        let (tx_oneshot, rx_oneshot) = oneshot::channel();
        tx.send(DiskMsg::RequestBlocks((2, tx_oneshot)))
            .await
            .unwrap();

        let expected = VecDeque::from([
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

        let result = rx_oneshot.await.unwrap();
        assert_eq!(result, expected);

        let requested = torrent.ctx.requested_blocks.read().await;

        let expected = VecDeque::from([
            BlockInfo {
                index: 0,
                begin: 0,
                len: 16384,
            },
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

        assert_eq!(*requested, expected);
        drop(requested);
    }

    // given a `BlockInfo`, we must be to read the right file,
    // at the right offset.
    // when we seed to other peers, this is how it will work.
    // when we get the bytes, it's easy to just create a Block from a BlockInfo.
    #[tokio::test]
    async fn multi_file_write_read_block() {
        let (torrent_tx, torrent_rx) = mpsc::channel::<TorrentMsg>(1);
        let (disk_tx, _) = mpsc::channel::<DiskMsg>(1);

        let m = get_magnet("magnet:?xt=urn:btih:48aac768a865798307ddd4284be77644368dd2c7&dn=Kerkour%20S.%20Black%20Hat%20Rust...Rust%20programming%20language%202022&tr=udp%3A%2F%2Ftracker.coppersurfer.tk%3A6969%2Fannounce&tr=udp%3A%2F%2F9.rarbg.to%3A2920%2Fannounce&tr=udp%3A%2F%2Ftracker.opentrackr.org%3A1337&tr=udp%3A%2F%2Ftracker.internetwarriors.net%3A1337%2Fannounce&tr=udp%3A%2F%2Ftracker.leechers-paradise.org%3A6969%2Fannounce&tr=udp%3A%2F%2Ftracker.pirateparty.gr%3A6969%2Fannounce&tr=udp%3A%2F%2Ftracker.cyberia.is%3A6969%2Fannounce").unwrap();
        let torrent = Torrent::new(torrent_tx, disk_tx, torrent_rx, m).await;
        let torrent_ctx = torrent.ctx.clone();

        let (tx, rx) = mpsc::channel(5);
        let mut disk = Disk::new(rx, torrent_ctx);

        let info = Info {
            file_length: None,
            name: "arch".to_owned(),
            piece_length: 6,
            pieces: vec![0],
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

        spawn(async move {
            disk.run().await.unwrap();
        });

        tx.send(DiskMsg::NewTorrent(info)).await.unwrap();

        //
        //  WRITE BLOCKS
        //

        // write a block before reading it
        // write entire first file
        let block = Block {
            index: 0,
            begin: 0,
            block: vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12],
        };

        let (tx_oneshot, rx_oneshot) = oneshot::channel();
        tx.send(DiskMsg::WriteBlock((block.clone(), tx_oneshot)))
            .await
            .unwrap();
        let result = rx_oneshot.await.unwrap();
        assert!(result.is_ok());

        // validate that the first file contains the bytes that we wrote
        let block_info = BlockInfo {
            index: 0,
            begin: 0,
            len: 12,
        };
        let (tx_oneshot, rx_oneshot) = oneshot::channel();
        tx.send(DiskMsg::ReadBlock((block_info, tx_oneshot)))
            .await
            .unwrap();
        let result = rx_oneshot.await.unwrap();
        assert_eq!(result.unwrap(), block.block);

        // write a block before reading it
        // write entire second file
        let block = Block {
            index: 2,
            begin: 0,
            block: vec![13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24],
        };

        let (tx_oneshot, rx_oneshot) = oneshot::channel();
        tx.send(DiskMsg::WriteBlock((block.clone(), tx_oneshot)))
            .await
            .unwrap();
        let result = rx_oneshot.await.unwrap();
        assert!(result.is_ok());

        // validate that the second file contains the bytes that we wrote
        let block_info = BlockInfo {
            index: 2,
            begin: 0,
            len: 12,
        };
        let (tx_oneshot, rx_oneshot) = oneshot::channel();
        tx.send(DiskMsg::ReadBlock((block_info, tx_oneshot)))
            .await
            .unwrap();
        let result = rx_oneshot.await.unwrap();
        assert_eq!(result.unwrap(), block.block);

        // write a block before reading it
        // write entire third file
        let block = Block {
            index: 4,
            begin: 0,
            block: vec![25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36],
        };

        let (tx_oneshot, rx_oneshot) = oneshot::channel();
        tx.send(DiskMsg::WriteBlock((block.clone(), tx_oneshot)))
            .await
            .unwrap();
        let result = rx_oneshot.await.unwrap();
        assert!(result.is_ok());

        // validate that the third file contains the bytes that we wrote
        let block_info = BlockInfo {
            index: 4,
            begin: 0,
            len: 12,
        };
        let (tx_oneshot, rx_oneshot) = oneshot::channel();
        tx.send(DiskMsg::ReadBlock((block_info, tx_oneshot)))
            .await
            .unwrap();
        let result = rx_oneshot.await.unwrap();
        assert_eq!(result.unwrap(), block.block);

        //
        //  READ BLOCKS
        //

        let block_info = BlockInfo {
            index: 1,
            begin: 0,
            len: 3,
        };

        // read piece 0 block from first file
        let (tx_oneshot, rx_oneshot) = oneshot::channel();
        tx.send(DiskMsg::ReadBlock((block_info, tx_oneshot)))
            .await
            .unwrap();
        let result = rx_oneshot.await.unwrap();
        assert_eq!(result.unwrap(), vec![7, 8, 9]);

        let block_info = BlockInfo {
            index: 0,
            begin: 1,
            len: 3,
        };

        // read piece 1 block from first file
        let (tx_oneshot, rx_oneshot) = oneshot::channel();
        tx.send(DiskMsg::ReadBlock((block_info, tx_oneshot)))
            .await
            .unwrap();
        let result = rx_oneshot.await.unwrap();
        assert_eq!(result.unwrap(), vec![2, 3, 4]);

        let block_info = BlockInfo {
            index: 2,
            begin: 0,
            len: 3,
        };

        // read piece 2 block from second file
        let (tx_oneshot, rx_oneshot) = oneshot::channel();
        tx.send(DiskMsg::ReadBlock((block_info, tx_oneshot)))
            .await
            .unwrap();
        let result = rx_oneshot.await.unwrap();
        assert_eq!(result.unwrap(), vec![13, 14, 15]);

        let block_info = BlockInfo {
            index: 2,
            begin: 1,
            len: 6,
        };

        // read piece 2 block from second file
        let (tx_oneshot, rx_oneshot) = oneshot::channel();
        tx.send(DiskMsg::ReadBlock((block_info, tx_oneshot)))
            .await
            .unwrap();
        let result = rx_oneshot.await.unwrap();
        assert_eq!(result.unwrap(), vec![14, 15, 16, 17, 18, 19]);

        let block_info = BlockInfo {
            index: 4,
            begin: 0,
            len: 6,
        };

        // read piece 3 block from third file
        let (tx_oneshot, rx_oneshot) = oneshot::channel();
        tx.send(DiskMsg::ReadBlock((block_info, tx_oneshot)))
            .await
            .unwrap();
        let result = rx_oneshot.await.unwrap();
        assert_eq!(result.unwrap(), vec![25, 26, 27, 28, 29, 30]);
    }
}
