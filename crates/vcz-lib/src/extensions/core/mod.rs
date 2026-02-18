//! Documentation of the "TCP Wire" protocol between Peers in the network.
//! Peers will follow this protocol to exchange information about torrents.

mod codec;
mod handshake_codec;

// re-exports
pub use codec::*;
pub use handshake_codec::*;

use bytes::Bytes;

/// The default block_len that most clients support, some clients drop
/// the connection on blocks larger than this value.
///
/// Tha last block of a piece might be smaller.
pub const BLOCK_LEN: usize = 16384;

/// Protocol String (PSTR)
/// Bytes of the string "BitTorrent protocol". Used during handshake.
pub const PSTR: [u8; 19] = [
    66, 105, 116, 84, 111, 114, 114, 101, 110, 116, 32, 112, 114, 111, 116,
    111, 99, 111, 108,
];

pub const PSTR_LEN: usize = 19;

/// A Block is a subset of a Piece,
/// pieces are subsets of the entire Torrent data.
///
/// Blocks may overlap pieces, for example, part of a block may start at piece
/// 0, but end at piece 1.
///
/// When peers send data (seed) to us, they send us Blocks.
/// This happens on the "Piece" message of the peer wire protocol.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Block {
    /// The index of the piece this block belongs to.
    pub index: usize,

    /// The zero-based byte offset into the piece.
    pub begin: usize,

    /// The block's data. 16 KiB most of the times,
    /// but the last block of a piece *might* be smaller.
    pub block: Bytes,
}

impl Block {
    /// Validate the [`Block`]. Like most clients, we only support
    /// data <= 16kiB.
    #[inline]
    pub fn is_valid(&self) -> bool {
        self.block.len() <= BLOCK_LEN && self.begin <= BLOCK_LEN
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
    pub index: usize,

    /// The zero-based byte offset into the piece.
    pub begin: usize,

    /// The block's length in bytes. <= 16 KiB
    pub len: usize,
}

impl Default for BlockInfo {
    fn default() -> Self {
        Self { index: 0, begin: 0, len: BLOCK_LEN }
    }
}

impl From<&Block> for BlockInfo {
    fn from(block: &Block) -> Self {
        BlockInfo { index: block.index, begin: block.begin, len: block.block.len() }
    }
}

impl BlockInfo {
    pub fn new(index: usize, begin: usize, len: usize) -> Self {
        Self { index, begin, len }
    }

    /// Validate the [`BlockInfo`]. Like most clients, we only support
    /// data <= 16kiB.
    #[inline]
    pub fn is_valid(&self) -> bool {
        self.len > 0 && self.len <= BLOCK_LEN && self.begin <= BLOCK_LEN
    }
}
