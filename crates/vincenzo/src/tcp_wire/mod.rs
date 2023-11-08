//! Documentation of the "TCP Wire" protocol between Peers in the network.
//! Peers will follow this protocol to exchange information about torrents.
pub mod messages;

use bytes::BufMut;
use bytes::BytesMut;
use tokio::io;

/// The default block_len that most clients support, some clients drop
/// the connection on blocks larger than this value.
///
/// Tha last block of a piece might be smallar.
pub const BLOCK_LEN: u32 = 16384;

/// Protocol String
/// String identifier of the string "BitTorrent protocol", in bytes.
pub const PSTR: [u8; 19] = [
    66, 105, 116, 84, 111, 114, 114, 101, 110, 116, 32, 112, 114, 111, 116, 111, 99, 111, 108,
];

/// A Block is a subset of a Piece,
/// pieces are subsets of the entire Torrent data.
///
/// When peers send data (seed) to us, they send us Blocks.
/// This happens on the "Piece" message of the peer wire protocol.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Block {
    /// The index of the piece this block belongs to.
    pub index: usize,
    /// The zero-based byte offset into the piece.
    pub begin: u32,
    /// The block's data. 16 KiB most of the times,
    /// but the last block of a piece *might* be smaller.
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

    /// Validate the [`Block`]. Like most clients, we only support
    /// data <= 16kiB.
    pub fn is_valid(&self) -> bool {
        self.block.len() <= BLOCK_LEN as usize && self.begin <= BLOCK_LEN
    }
}

/// The representation of a [`Block`].
///
/// When we ask a peer to give us a [`Block`], we send this struct,
/// using the "Request" message of the tcp wire protocol.
///
/// This is almost identical to the [`Block`] struct,
/// the only difference is that instead of having a `block`,
/// we have a `len` representing the len of the block.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlockInfo {
    /// The index of the piece of which this is a block.
    pub index: u32,
    /// The zero-based byte offset into the piece.
    pub begin: u32,
    /// The block's length in bytes. <= 16 KiB
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

impl From<Block> for BlockInfo {
    fn from(val: Block) -> Self {
        BlockInfo {
            index: val.index as u32,
            begin: val.begin,
            len: val.block.len() as u32,
        }
    }
}

impl BlockInfo {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn index(mut self, index: u32) -> Self {
        self.index = index;
        self
    }
    pub fn begin(mut self, begin: u32) -> Self {
        self.begin = begin;
        self
    }
    pub fn len(mut self, len: u32) -> Self {
        self.len = len;
        self
    }
    /// Encodes the block info in the network binary protocol's format into the
    /// given buffer.
    pub fn encode(&self, buf: &mut BytesMut) -> io::Result<()> {
        buf.put_u32(self.index);
        buf.put_u32(self.begin);
        buf.put_u32(self.len);
        Ok(())
    }
    /// Validate the [`BlockInfo`]. Like most clients, we only support
    /// data <= 16kiB.
    pub fn is_valid(&self) -> bool {
        self.len <= BLOCK_LEN && self.begin <= BLOCK_LEN && self.len > 0
    }
}
