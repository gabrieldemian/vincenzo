//! An implementation of the Bitfield data format, used by BitTorrent protocol.
use bitvec::prelude::*;

pub type Bitfield = BitVec<u8, Msb0>;
pub type Reserved = BitArray<[u8; 8], Msb0>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_bitvec() {
        let a = bitvec![u8, Msb0; 0; 0 as usize];
        println!("a {a:#?}");
        println!("len {:#?}", a.len());

        assert!(true);
    }
}
