//! Wrapper types around Bitvec.
use bitvec::{prelude::*, ptr::Const};

/// Bitfield where index = piece.
pub type Bitfield = BitVec<u8, Msb0>;

/// Reserved bytes exchanged during handshake.
type ReservedAlias = BitArray<[u8; 8], bitvec::prelude::Msb0>;

#[derive(Debug, Clone, Default, Copy, PartialEq, Eq)]
pub struct Reserved(pub ReservedAlias);

impl From<[u8; 8]> for Reserved {
    fn from(value: [u8; 8]) -> Self {
        Self(ReservedAlias::from(value))
    }
}

impl Reserved {
    /// Reserved bits of protocols that the client supports.
    pub fn supported() -> Reserved {
        // we only support the `extension protocol`
        Reserved(bitarr![u8, Msb0;
            0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 1, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0,
        ])
    }

    pub fn supports_extended(&self) -> bool {
        unsafe { *self.0.get_unchecked(43) }
    }
}

pub trait VczBitfield {
    fn from_piece(piece: usize) -> Bitfield {
        bitvec![u8, Msb0; 0; piece]
    }
    /// Set vector to a new len, in bits.
    fn new_and_resize(vec: Vec<u8>, len: usize) -> Bitfield {
        let mut s = Bitfield::from_vec(vec);
        unsafe { s.set_len(len) };
        s
    }
    fn safe_get(&mut self, index: usize) -> BitRef<'_, Const, u8, Msb0>;
    fn safe_set(&mut self, _index: usize) {}
}

impl VczBitfield for Bitfield {
    fn safe_set(&mut self, index: usize) {
        if self.len() <= index {
            self.resize(index + 1, false);
        }
        unsafe { self.set_unchecked(index, true) };
    }
    fn safe_get(&mut self, index: usize) -> BitRef<'_, Const, u8, Msb0> {
        if self.len() <= index {
            self.resize(index + 1, false);
        }
        unsafe { self.get_unchecked(index) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_piece() {
        let bitfield = Bitfield::from_piece(1407);
        assert_eq!(bitfield.len(), 1407);
    }

    #[test]
    fn safe_set() {
        // 0, 1
        let mut bitfield = Bitfield::new_and_resize(vec![0], 2);
        assert_eq!(bitfield.len(), 2);
        // 0, 1, 2
        bitfield.safe_set(2);
        assert_eq!(bitfield.len(), 3);
        assert_eq!(bitfield.get(2).unwrap(), true);

        bitfield.safe_set(10);
        assert_eq!(bitfield.len(), 11);
        assert_eq!(bitfield.get(10).unwrap(), true);
    }

    #[test]
    fn safe_get() {
        let mut bitfield = Bitfield::new_and_resize(vec![0], 1);
        assert_eq!(bitfield.safe_get(10), false);
        assert_eq!(bitfield.len(), 11);
        assert_eq!(bitfield.get(10).unwrap(), false);
    }

    #[test]
    fn supports_ext() {
        let bitfield = Reserved::supported();
        assert!(bitfield.supports_extended());
    }

    #[test]
    fn new_and_resize() {
        let bitfield = Bitfield::new_and_resize(vec![0], 5);
        assert_eq!(bitfield.len(), 5);

        let bitfield = Bitfield::new_and_resize(vec![0], 7);
        assert_eq!(bitfield.len(), 7);

        let bitfield = Bitfield::new_and_resize(vec![0], 8);
        assert_eq!(bitfield.len(), 8);

        let bitfield = Bitfield::new_and_resize(vec![0, 0], 9);
        assert_eq!(bitfield.len(), 9);
    }
}
