use std::{collections::VecDeque, io::SeekFrom};

use log::{debug, info};
use tokio::{
    fs::{create_dir_all, File, OpenOptions},
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
    sync::{mpsc::Receiver, oneshot::Sender},
};

use crate::{
    error::Error,
    metainfo::Info,
    tcp_wire::lib::{Block, BlockInfo, BLOCK_LEN},
};

#[derive(Debug)]
pub enum DiskMsg {
    NewTorrent(Info),
    ReadBlock((BlockInfo, Sender<Vec<u8>>)),
    OpenFile((String, Sender<File>)),
    WriteBlock((Block, Sender<Result<(), Error>>)),
}

#[derive(Debug)]
pub struct Disk {
    rx: Receiver<DiskMsg>,
    pub ctx: DiskCtx,
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
    pub fn new(rx: Receiver<DiskMsg>) -> Self {
        let ctx = DiskCtx {
            // base_path: "~/Downloads/".to_string(),
            base_path: "".to_string(),
            ..Default::default()
        };
        Self { rx, ctx }
    }

    pub async fn run(&mut self) -> Result<(), Error> {
        debug!("running Disk event loop");

        while let Some(msg) = self.rx.recv().await {
            match msg {
                DiskMsg::NewTorrent(info) => {
                    self.ctx.block_infos = info.clone().into();
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
                                self.open_file(&format!(
                                    "{:}/{dir_path}{:}",
                                    &self.ctx.info.name, &file_ext,
                                ))
                                .await?;
                            }
                        }
                    }

                    if let Some(_) = self.ctx.info.file_length {
                        self.open_file(&self.ctx.info.name).await?;
                    }
                }
                DiskMsg::ReadBlock((block_info, tx)) => {
                    let bytes = self.read_block(block_info).await?;
                    tx.send(bytes).unwrap();
                }
                DiskMsg::WriteBlock((block, tx)) => {
                    let bytes = self.write_block(block).await;
                    tx.send(bytes).unwrap();
                }
                DiskMsg::OpenFile((path, tx)) => {
                    let file = self.open_file(&path).await?;
                    let _ = tx.send(file);
                }
            }
        }

        Ok(())
    }

    pub async fn open_file(&self, path: &str) -> Result<File, Error> {
        return OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(&format!("{:}{path}", self.ctx.base_path))
            .await
            .map_err(|_| Error::FileOpenError);
    }

    pub async fn read_block(&self, block_info: BlockInfo) -> Result<Vec<u8>, Error> {
        let path = "foo.txt";
        // let path = "../Kerkour S. Black Hat Rust...Rust programming language 2022../Kerkour S. Black Hat Rust...Rust programming language 2022.pdf";
        let mut file = self.open_file(path).await?;

        // advance buffer until the start of the offset
        file.seek(SeekFrom::Start(block_info.index as u64 * BLOCK_LEN as u64))
            .await
            .unwrap();

        // how many bytes to read, after offset (begin)
        // it will be 16KiB for all blocks, except the last one
        // which may be smaller
        let mut buf = vec![0; block_info.len as usize];

        // how many bytes were read, if <= BLOCK_LEN
        // then we know it's the last block
        // or if the index is the last index
        file.read_exact(&mut buf).await?;

        Ok(buf)
    }

    pub async fn write_block(&self, block: Block) -> Result<(), Error> {
        let path = "foo.txt";
        // let path = "../Kerkour S. Black Hat Rust...Rust programming language 2022../Kerkour S. Black Hat Rust...Rust programming language 2022.pdf";
        let mut file = self.open_file(path).await?;

        // advance buffer until the start of the offset
        file.seek(SeekFrom::Start(block.index as u64 * BLOCK_LEN as u64))
            .await?;

        file.write_all(&block.block).await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use bendy::decoding::FromBencode;
    use std::{
        collections::VecDeque,
        io::{Cursor, SeekFrom},
        sync::Arc,
    };

    use crate::{
        metainfo::{File, Info, MetaInfo},
        tcp_wire::lib::Block,
    };

    use super::*;
    use tokio::{
        fs::create_dir_all,
        io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
        spawn,
        sync::{mpsc, oneshot, Mutex},
    };

    #[tokio::test]
    async fn create_file_tree() {
        // # examples
        // piece_len: 262144 2.6MiB
        // block_len:  16384 16KiB
        // qnt pieces: 10
        // 16 blocks in 1 piece
        //
        // block_len: 2 byte
        // piece_len: 4 bytes
        // file 1 len: 5 + half of the last block
        // file 2 len: 5 + half of the first block
        //
        // # strategy 1
        // get equal sized blocks but split them between pieces
        // |----p0----|----p1----|
        // |  b0  |  b1  |  b2   |
        // |----------|-----------
        // |  file|0  |  |file1  |
        // -----------------------
        //    5bytes     5bytes
        //
        // # strategy 2
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
        let path = "Kerkour S. Black Hat Rust...Rust programming language 2022/Kerkour S. Black Hat Rust...Rust programming language 2022.pdf";

        let (tx, rx) = mpsc::channel(5);
        let mut disk = Disk::new(rx);

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
    async fn send_new_torrent_msg() {
        let (tx, rx) = mpsc::channel(1);
        let mut disk = Disk::new(rx);

        spawn(async move {
            disk.run().await.unwrap();
        });

        let info = Info::default();

        tx.send(DiskMsg::NewTorrent(info)).await.unwrap();

        std::fs::remove_file("foo.txt").unwrap();
    }

    // test that we can read individual, out of order, pieces
    // of a file
    #[tokio::test]
    async fn read_piece() {
        let (_, rx) = mpsc::channel(1);
        let mut disk = Disk::new(rx);
        let info = Info::default();
        disk.ctx.info = info;

        let path = "foo.txt";
        // let path = "../Kerkour S. Black Hat Rust...Rust programming language 2022../Kerkour S. Black Hat Rust...Rust programming language 2022.pdf";
        let mut file = disk.open_file(path).await.unwrap();

        // in reality, the block_len is 16 KiB
        // i can use the block_len to split the file buffer
        // and read and write in a non-sequential way
        let block_len: u64 = 3;

        let block_info = BlockInfo {
            index: 1,
            begin: 0,
            len: 3,
        };

        // when we write, we also advance the cursor of the buffer
        file.write_all_buf(&mut Cursor::new(&[255, 120, 50, 35, 2, 76, 30, 12, 4]))
            .await
            .unwrap();

        // advance buffer until the start of the offset
        // since I'm only using blocks with 16KiB
        // and rejecting pieces larget than that,
        // it's safe to make this calculation to get the piece offset
        // with the only exception being the last piece/block which
        // may be smallar than 16 KiB
        file.seek(SeekFrom::Start(block_len * block_info.index as u64))
            .await
            .unwrap();

        let mut buf = vec![0; block_info.len as usize];
        file.read_exact(&mut buf).await.unwrap();

        assert_eq!(buf, &[35, 2, 76]);

        tokio::fs::remove_file("foo.txt").await.unwrap();
    }

    // test that we can write pieces in the middle of the file buffer
    // and return the bytes written
    #[tokio::test]
    async fn write_piece() {
        let (_, rx) = mpsc::channel(1);
        let mut disk = Disk::new(rx);
        let info = Info::default();
        disk.ctx.info = info;

        let path = "foo.txt";
        let mut file = disk.open_file(path).await.unwrap();

        // in reality, the block_len is 16 KiB
        // i can use the block_len to split the file buffer
        // and read and write in a non-sequential way
        let block_len: u64 = 3;

        let block = Block {
            index: 1,
            begin: 0,
            block: vec![35, 2, 76],
        };

        file.write_all_buf(&mut Cursor::new(&[0; 9])).await.unwrap();

        // advance buffer until the start of the offset
        file.seek(SeekFrom::Start(block_len * block.index as u64))
            .await
            .unwrap();

        // when we write, we also advance the cursor of the buffer
        file.write_all(&block.block).await.unwrap();

        // now we go back again to the start of the offset
        file.seek(SeekFrom::Start(block_len * block.index as u64))
            .await
            .unwrap();

        let mut buf = vec![0; block_len as usize];
        file.read_exact(&mut buf).await.unwrap();

        assert_eq!(buf, block.block);

        tokio::fs::remove_file("foo.txt").await.unwrap();
    }

    // simulate seeding. a Peer will send a block info and expect a Block in return,
    // we must read the bytes from disk and construct the Block.
    #[test]
    fn read_bytes_from_torrent() -> Result<(), Error> {
        let index: u32 = 0;
        let piece_len: u32 = 262_144;
        let piece_begin = index * piece_len;
        let piece_end = piece_begin + piece_len;

        // multi file info
        let metainfo = include_bytes!("../book.torrent");
        let metainfo = MetaInfo::from_bencode(metainfo).unwrap();
        let info = metainfo.info;

        let torrent_len: u32 = info
            .files
            .as_ref()
            .unwrap()
            .iter()
            .fold(0, |acc, f| acc * f.length);

        println!("torrent_len: {torrent_len:#?}");
        println!("piece index: {index:#?}");
        println!("piece_begin: {piece_begin:#?}");
        println!("piece_end: {piece_end:#?}");

        // find a file on a list of files,
        // given a piece_index and a piece_len
        let (_, file) = info
            .files
            .as_ref()
            .unwrap()
            .into_iter()
            .enumerate()
            .find(|(i, f)| piece_begin < f.length * (*i as u32 + 1))
            .unwrap();

        println!("found file: {file:#?}");

        let bis: VecDeque<BlockInfo> = info.into();

        println!("file to disk_file: {bis:#?}");

        let block_info = BlockInfo {
            len: BLOCK_LEN,
            index: 0,
            begin: 0,
        };

        Ok(())
    }
}
