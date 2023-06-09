use bytes::BufMut;
use bytes::BytesMut;
use tokio::io;

/// This is the only block length we're dealing with (except for possibly the
/// last block).  It is the widely used and accepted 16 KiB.
pub(crate) const BLOCK_LEN: u32 = 16384;

/// Protocol String
/// String identifier of the BitTorrent protocol, in bytes.
pub(crate) const PSTR: [u8; 19] = [
    66, 105, 116, 84, 111, 114, 114, 101, 110, 116, 32, 112, 114, 111, 116, 111, 99, 111, 108,
];

/// Request message will send this BlockInfo
/// A block is a fixed size chunk of a piece, which in turn is a fixed size
/// chunk of a torrent. Downloading torrents happen at this block level
/// granularity.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlockInfo {
    /// The index of the piece of which this is a block.
    pub index: u32,
    /// The zero-based byte offset into the piece.
    pub begin: u32,
    /// The block's length in bytes. Always 16 KiB (0x4000 bytes) or less, for
    /// now.
    pub len: u32,
}

impl Default for BlockInfo {
    fn default() -> Self {
        Self {
            index: 0,
            begin: 0,
            len: BLOCK_LEN,
        }
    }
}

impl BlockInfo {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn index(mut self, index: u32) -> Self {
        self.index = u32::to_be(index);
        self
    }
    pub fn begin(mut self, begin: u32) -> Self {
        self.begin = u32::to_be(begin);
        self
    }
    pub fn len(mut self, len: u32) -> Self {
        self.len = u32::to_be(len);
        self
    }
    /// Encodes the block info in the network binary protocol's format into the
    /// given buffer.
    pub fn encode(&self, buf: &mut BytesMut) -> io::Result<()> {
        let piece_index = self
            .index
            .try_into()
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        buf.put_u32(piece_index);
        buf.put_u32(self.begin);
        buf.put_u32(self.len);
        Ok(())
    }
}

/// Piece message will send this Block
/// A block is a fixed size chunk of a piece, which in turn is a fixed size
/// chunk of a torrent. Downloading torrents happen at this block level
/// granularity.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Block {
    /// The index of the piece of which this is a block.
    pub index: usize,
    /// The zero-based byte offset into the piece.
    pub begin: u32,
    /// The block's length in bytes. Always 16 KiB (0x4000 bytes) or less, for
    /// now.
    pub block: Vec<u8>,
}

impl Block {
    /// Encodes the block info in the network binary protocol's format into the
    /// given buffer.
    pub fn encode(&self, buf: &mut BytesMut) -> io::Result<()> {
        let piece_index = self
            .index
            .try_into()
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        buf.put_u32(piece_index);
        buf.put_u32(self.begin);
        buf.extend_from_slice(&self.block);
        Ok(())
    }
}
