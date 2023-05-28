/// This is the only block length we're dealing with (except for possibly the
/// last block).  It is the widely used and accepted 16 KiB.
pub(crate) const BLOCK_LEN: u32 = 0x4000;

/// A block is a fixed size chunk of a piece, which in turn is a fixed size
/// chunk of a torrent. Downloading torrents happen at this block level
/// granularity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct BlockInfo {
    /// The index of the piece of which this is a block.
    pub piece_index: usize,
    /// The zero-based byte offset into the piece.
    pub offset: u32,
    /// The block's length in bytes. Always 16 KiB (0x4000 bytes) or less, for
    /// now.
    pub len: u32,
}
