use std::{collections::VecDeque, io::SeekFrom};

use log::{debug, info};
use tokio::{
    fs::{File, OpenOptions},
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
    sync::{mpsc::Receiver, oneshot::Sender},
};

use crate::{
    error::Error,
    tcp_wire::lib::{Block, BlockInfo, BLOCK_LEN},
};

#[derive(Debug)]
pub enum DiskMsg {
    NewTorrent { name: String },
    ReadBlock((BlockInfo, Sender<Vec<u8>>)),
    WriteBlock((Block, Sender<Result<(), Error>>)),
}

#[derive(Debug)]
pub struct Disk {
    rx: Receiver<DiskMsg>,
    pub ctx: DiskCtx,
}

#[derive(Debug)]
pub struct DiskCtx {
    pub file_path: Option<String>,
    pub base_path: String,
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
            file_path: None,
            base_path: "~/Downloads/".to_string(),
        };
        Self { rx, ctx }
    }

    pub async fn run(&mut self) -> Result<(), Error> {
        debug!("running Disk event loop");

        while let Some(msg) = self.rx.recv().await {
            match msg {
                DiskMsg::NewTorrent { name, .. } => {
                    self.ctx.file_path = Some(name);
                    self.open_file().await?;
                }
                DiskMsg::ReadBlock((block_info, tx)) => {
                    let bytes = self.read_block(block_info).await?;
                    tx.send(bytes).unwrap();
                }
                DiskMsg::WriteBlock((block, tx)) => {
                    let bytes = self.write_block(block).await;
                    tx.send(bytes).unwrap();
                }
            }
        }

        Ok(())
    }

    pub async fn open_file(&self) -> Result<File, Error> {
        if let Some(path) = &self.ctx.file_path {
            return OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .open(path)
                .await
                .map_err(|_| Error::FileOpenError);
        }
        Err(Error::FileOpenError)
    }

    pub async fn read_block(&self, block_info: BlockInfo) -> Result<Vec<u8>, Error> {
        let mut file = self.open_file().await?;

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
        let mut file = self.open_file().await?;

        // advance buffer until the start of the offset
        file.seek(SeekFrom::Start(block.index as u64 * BLOCK_LEN as u64))
            .await?;

        file.write_all(&block.block).await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        io::{Cursor, SeekFrom},
    };

    use crate::{
        metainfo::{File, Info},
        tcp_wire::lib::Block,
    };

    use super::*;
    use tokio::{
        fs::create_dir_all,
        io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
        spawn,
        sync::mpsc,
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
        // the last and first block of a piece may be a different size than block_len
    }

    #[tokio::test]
    async fn send_new_torrent_msg() {
        let (tx, rx) = mpsc::channel(1);
        let mut disk = Disk::new(rx);

        spawn(async move {
            disk.run().await.unwrap();
        });

        tx.send(DiskMsg::NewTorrent {
            name: "foo.txt".to_string(),
        })
        .await
        .unwrap();

        std::fs::remove_file("foo.txt").unwrap();
    }

    // test that we can read individual, out of order, pieces
    // of a file
    #[tokio::test]
    async fn read_piece() {
        let (_, rx) = mpsc::channel(1);
        let mut disk = Disk::new(rx);
        disk.ctx.file_path = Some("foo.txt".to_string());

        let mut file = disk.open_file().await.unwrap();

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
        disk.ctx.file_path = Some("foo.txt".to_string());

        let mut file = disk.open_file().await.unwrap();

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
}
