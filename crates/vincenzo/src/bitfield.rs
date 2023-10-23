//! Wrapper types around Bitvec.
use bitvec::prelude::*;

/// Bitfield where index = piece.
pub type Bitfield = BitVec<u8, Msb0>;

/// Reserved bytes exchanged during handshake.
pub type Reserved = BitArray<[u8; 8], Msb0>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_bitvec() {
        let a = bitvec![u8, Msb0; 0; 1];
        // a.set(9, true);
        println!("a {a:#?}");
        println!("len {:#?}", a.len());

        assert!(true);
    }
}
