use std::io::SeekFrom;

use log::info;
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
    NewTorrent { id: u32, name: String },
    ReadBlock((BlockInfo, Sender<Vec<u8>>)),
    WriteBlock((Block, Sender<()>)),
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

impl Disk {
    pub fn new(rx: Receiver<DiskMsg>) -> Self {
        let ctx = DiskCtx {
            file_path: None,
            base_path: "~/Downloads/".to_string(),
        };
        Self { rx, ctx }
    }

    pub async fn run(&mut self) -> Result<(), Error> {
        info!("running Disk event loop");

        while let Some(msg) = self.rx.recv().await {
            match msg {
                DiskMsg::NewTorrent { name, .. } => {
                    self.ctx.file_path = Some(name);
                }
                DiskMsg::ReadBlock((block_info, tx)) => {
                    let bytes = self.read_block(block_info).await?;
                    tx.send(bytes).unwrap();
                }
                DiskMsg::WriteBlock((block, tx)) => {
                    let bytes = self.write_block(block).await?;
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
            .await
            .unwrap();

        file.write_all(&block.block).await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, SeekFrom};

    use crate::tcp_wire::lib::Block;

    use super::*;
    use tokio::{
        io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
        sync::mpsc,
    };

    #[tokio::test]
    async fn create_file() {
        let (_, rx) = mpsc::channel(1);
        let mut disk = Disk::new(rx);
        disk.ctx.file_path = Some("foo.txt".to_string());

        let file = disk.open_file().await;

        assert!(file.is_ok());

        tokio::fs::remove_file("foo.txt").await.unwrap();
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
