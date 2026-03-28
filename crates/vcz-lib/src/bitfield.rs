//! Wrapper types around Bitvec.
use bit_vec::BitVec;

/// Bitfield where index = piece.
pub type Bitfield = BitVec<u8>;

#[derive(Debug, Clone, Default, Copy, PartialEq, Eq)]
pub struct Reserved(pub [u8; 8]);

impl From<[u8; 8]> for Reserved {
    fn from(value: [u8; 8]) -> Self {
        Self(value)
    }
}

impl Reserved {
    /// Reserved bits of protocols that the client supports.
    pub fn supported() -> Reserved {
        // we only support the `extension protocol`
        Reserved([0, 0, 0, 0, 0, 0b00010000, 0, 0])
    }

    #[inline]
    pub fn supports_extended(&self) -> bool {
        (self.0[5] & 0x10) != 0
    }
}

pub(crate) trait VczBitfield {
    fn from_piece(piece: usize) -> Bitfield {
        bitvec![u8, Msb0; 0; piece]
    }
    /// Set vector to a new len, in bits.
    #[cfg(test)]
    fn new_and_resize(vec: Vec<u8>, len: usize) -> Bitfield {
        let mut s = Bitfield::from_vec(vec);
        unsafe { s.set_len(len) };
        s
    }
    fn safe_get(
        &mut self,
        index: usize,
    ) -> BitRef<'_, bitvec::ptr::Const, u8, Msb0>;
    fn safe_set(&mut self, index: usize, val: bool);
}

impl VczBitfield for BitVec {
    fn safe_set(&mut self, index: usize, val: bool) {
        if index >= self.len() {
            let needed = index + 1 - self.len();
            self.extend(BitVec::from_elem(needed, false));
        }
        self.set(index, val);
    }

    fn safe_get(&mut self, index: usize) -> bool {
        if index >= self.len() {
            let needed = index + 1 - self.len();
            self.extend(BitVec::from_elem(needed, false));
        }
        unsafe { self.get_unchecked(index) }
    }
}

impl VczBitfield for Bitfield {
    fn safe_set(&mut self, index: usize, val: bool) {
        if index >= self.len() {
            let needed = index + 1 - self.len();
            self.extend(BitVec::from_elem(needed, false));
        }
        self.set(index, val);
    }

    fn safe_get(&mut self, index: usize) -> bool {
        if index >= self.len() {
            let needed = index + 1 - self.len();
            self.extend(BitVec::from_elem(needed, false));
        }
        unsafe { self.get_unchecked(index) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_piece() {
        let bitfield = BitVec::from_elem(1407, false);
        assert_eq!(bitfield.len(), 1407);
    }

    #[test]
    fn safe_set() {
        // 0, 1
        let mut bitfield = BitVec::from_elem(2, false);
        assert_eq!(bitfield.len(), 2);
        // 0, 1, 2
        bitfield.safe_set(2, true);
        assert_eq!(bitfield.len(), 3);
        assert!(bitfield.get(2).unwrap());

        bitfield.safe_set(10, true);
        assert_eq!(bitfield.len(), 11);
        assert!(bitfield.get(10).unwrap());
    }

    #[test]
    fn safe_get() {
        let mut bitfield = BitVec::from_elem(1, true);
        bitfield.safe_set(10, true);
        assert!(bitfield.safe_get(10));
        assert_eq!(bitfield.len(), 11);
        assert!(bitfield.get(10).unwrap());
    }

    #[test]
    fn supports_ext() {
        let bitfield = Reserved::supported();
        assert!(bitfield.supports_extended());
    }
}
