use bitlab::*;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Bitfield {
    pub inner: Vec<u8>,
    curr: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BitItem {
    pub bit: u8,
    pub index: usize,
}

impl From<Vec<u8>> for Bitfield {
    fn from(value: Vec<u8>) -> Self {
        Self {
            inner: value,
            curr: 0,
        }
    }
}

// This iterator will return 0s and 1s
impl Iterator for Bitfield {
    type Item = BitItem;
    fn next(&mut self) -> Option<Self::Item> {
        let bit = self.get(self.curr);
        self.curr += 1;
        bit
    }
}

pub trait IntoIterator {
    type Item;
    type IntoIter: Iterator<Item = Self::Item>;
    fn into_iter(self) -> Self::IntoIter;
}

impl Bitfield {
    pub fn new() -> Self {
        Self::default()
    }

    /// Get byte and bit_index that correspond to the provided bit
    fn get_byte<I: Into<usize>>(&self, index: I) -> Option<(u8, usize, usize)> {
        // index of the bit
        let index: usize = index.into();
        // index of the slice, where the bit lives in
        let slice_index = index / 8;
        // position of the bit/index under slice_index
        let bit_index = index % 8;

        if self.inner.len() <= slice_index || index > self.len() {
            return None;
        }
        // byte where `index` lives in
        let byte = self.inner[slice_index];

        Some((byte, slice_index, bit_index))
    }

    /// Return true if the provided index is 1. False otherwise.
    pub fn has<I: Into<usize>>(&self, index: I) -> bool {
        if let Some((byte, _, bit_index)) = self.get_byte(index) {
            let r = byte.get_bit(bit_index as u32);
            if let Ok(r) = r {
                return r;
            }
            return false;
        }
        false
    }

    /// Get a bit and its index
    pub fn get<I: Copy + Into<usize>>(&self, index: I) -> Option<BitItem> {
        let (byte, _, bit_index) = self.get_byte(index)?;
        let r = byte.get_bit(bit_index as u32).unwrap();

        if r {
            Some(BitItem {
                bit: 1_u8,
                index: index.into(),
            })
        } else {
            Some(BitItem {
                bit: 0_u8,
                index: index.into(),
            })
        }
    }

    /// Set a bit, turn a 0 into a 1
    pub fn try_set<I: Into<usize>>(&mut self, index: I) -> Option<u8> {
        let (byte, slice_index, bit_index) = self.get_byte(index)?;
        let r = byte.set_bit(bit_index as u32);

        match r {
            Ok(r) => {
                self.inner[slice_index] = r;
                Some(r)
            }
            Err(_) => None,
        }
    }

    /// Set a bit. turn a 0 into 1. If the given index is out of bounds with the inner vec,
    /// The vec will be increased dynamically to fit in the bounds of the index.
    pub fn set(&mut self, index: usize) -> Option<u8> {
        // index of the bit
        let index: usize = index;
        // index of the slice, where the bit lives in
        let slice_index = index / 8;

        // try to set if it is inbound
        match self.try_set(index) {
            Some(a) => Some(a),
            None => {
                // index out of bounds here, will increase the inner vec
                // to accomodate the given `index`
                // need to grow the inner vec by this number of times
                let diff = slice_index - self.inner.len();
                for _ in 0..=diff {
                    self.inner.push(0);
                }
                self.set(index);
                Some(index as u8)
            }
        }
    }

    /// Clear a bit, turn a 1 into a 0
    pub fn clear<I: Into<usize>>(&mut self, index: I) -> Option<u8> {
        let (byte, slice_index, bit_index) = self.get_byte(index)?;
        let r = byte.clear_bit(bit_index as u32);

        match r {
            Ok(r) => {
                self.inner[slice_index] = r;
                Some(r)
            }
            Err(_) => None,
        }
    }
    pub fn is_complete(&mut self, pieces: u32) -> bool {
        let my_pieces: u32 = self.inner.iter().fold(0, |acc, byte| {
            let ones = *byte as f32 / 32_f32;
            let ones = ones.ceil();
            acc + ones as u32
        });
        println!("my_pieces {my_pieces}");
        my_pieces == pieces
    }
    /// Returns the lenght of inner as bits
    pub fn len(&self) -> usize {
        self.inner.len() * 8_usize
    }
    /// Returns the lenght of inner as bytes
    pub fn len_bytes(&self) -> usize {
        self.inner.len()
    }
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn can_create_from_vec() {
        let bitfield = Bitfield::from(vec![255_u8]);
        assert_eq!(bitfield.inner, vec![255_u8]);
    }

    #[test]
    fn can_complete_bitfield() {
        let mut bitfield = Bitfield::from(vec![0b11111111, 0b01111100]);
        let _is_complete = bitfield.is_complete(bitfield.len() as u32);
        // assert!(is_complete);
    }

    #[test]
    fn can_has_from_bitfield() {
        let bits: Vec<u8> = vec![0b10101010, 0b00011011, 0b00111110];
        let bitfield = Bitfield::from(bits);

        let index_a = bitfield.has(0_usize);
        let index_b = bitfield.has(3_usize);
        let index_c = bitfield.has(6_usize);
        let index_d = bitfield.has(7_usize);
        assert!(index_a);
        assert!(!index_b);
        assert!(index_c);
        assert!(!index_d);

        let index_a = bitfield.has(8_usize);
        let index_b = bitfield.has(9_usize);
        let index_c = bitfield.has(10_usize);
        let index_d = bitfield.has(11_usize);
        let index_e = bitfield.has(12_usize);
        assert!(!index_a);
        assert!(!index_b);
        assert!(!index_c);
        assert!(index_d);
        assert!(index_e);
        assert!(!index_c);
    }

    #[test]
    fn can_get_from_bitfield() {
        let bits: Vec<u8> = vec![0b1010_1010, 0b0001_1011, 0b0011_1110];
        let bitfield = Bitfield::from(bits);

        let index_a = bitfield.get(0_usize);
        let index_b = bitfield.get(3_usize);
        let index_c = bitfield.get(6_usize);
        let index_d = bitfield.get(7_usize);

        assert_eq!(index_a, Some(BitItem { bit: 1, index: 0 }));
        assert_eq!(index_b, Some(BitItem { bit: 0, index: 3 }));
        assert_eq!(index_c, Some(BitItem { bit: 1, index: 6 }));
        assert_eq!(index_d, Some(BitItem { bit: 0, index: 7 }));

        let index_a = bitfield.get(8_usize);
        let index_b = bitfield.get(9_usize);
        let index_c = bitfield.get(10_usize);
        let index_d = bitfield.get(11_usize);
        let index_e = bitfield.get(12_usize);

        assert_eq!(index_a, Some(BitItem { bit: 0, index: 8 }));
        assert_eq!(index_b, Some(BitItem { bit: 0, index: 9 }));
        assert_eq!(index_c, Some(BitItem { bit: 0, index: 10 }));
        assert_eq!(index_d, Some(BitItem { bit: 1, index: 11 }));
        assert_eq!(index_e, Some(BitItem { bit: 1, index: 12 }));

        let index_a = bitfield.get(23_usize);
        let index_b = bitfield.get(24_usize);
        assert_eq!(index_a, Some(BitItem { bit: 0, index: 23 }));
        assert_eq!(index_b, None);

        let index_a = bitfield.get(2000_usize);
        assert_eq!(index_a, None);
    }

    #[test]
    fn can_fail_get_bitfield() {
        let bits: Vec<u8> = vec![0b10101010];
        let bitfield = Bitfield::from(bits);

        let index_a = bitfield.has(8_usize);
        assert!(!index_a);

        let bits: Vec<u8> = vec![];
        let bitfield = Bitfield::from(bits);
        let index_a = bitfield.has(8_usize);
        assert!(!index_a);
    }

    #[test]
    fn can_set() {
        let mut bitfield = Bitfield::new();
        // [0..7] [8..15] [16..23] [24..31]
        bitfield.set(10_usize);
        assert_eq!(bitfield.len_bytes(), 2);
        assert_eq!(
            bitfield.get(10_usize).unwrap(),
            BitItem { index: 10, bit: 1 }
        );

        bitfield.set(23_usize);
        assert_eq!(bitfield.len_bytes(), 3);
        assert_eq!(
            bitfield.get(23_usize).unwrap(),
            BitItem { index: 23, bit: 1 }
        );

        bitfield.set(31_usize);
        assert_eq!(bitfield.len_bytes(), 4);
        assert_eq!(
            bitfield.get(31_usize).unwrap(),
            BitItem { index: 31, bit: 1 }
        );

        let mut bitfield = Bitfield::from(vec![0b0000_0000]);
        bitfield.set(2_usize);
        assert_eq!(bitfield.get(2_usize).unwrap(), BitItem { index: 2, bit: 1 });
        assert_eq!(bitfield.get(2_usize).unwrap(), BitItem { index: 2, bit: 1 });
        assert_eq!(bitfield.len_bytes(), 1);
    }

    #[test]
    fn can_try_set() {
        let bits: Vec<u8> = vec![0b0000_0000, 0b0000_0000];
        let mut bitfield = Bitfield::from(bits);

        bitfield.try_set(0_usize);
        assert_eq!(bitfield.inner[0], 0b1000_0000);
        assert_eq!(bitfield.get(0_usize), Some(BitItem { bit: 1, index: 0 }));

        bitfield.try_set(7_usize);
        assert_eq!(bitfield.inner[0], 0b1000_0001);
        assert_eq!(bitfield.get(7_usize), Some(BitItem { bit: 1, index: 7 }));

        bitfield.try_set(8_usize);
        assert_eq!(bitfield.inner[1], 0b1000_0000);
        assert_eq!(bitfield.get(8_usize), Some(BitItem { bit: 1, index: 8 }));

        bitfield.try_set(15_usize);
        assert_eq!(bitfield.inner[1], 0b1000_0001);
        assert_eq!(bitfield.get(15_usize), Some(BitItem { bit: 1, index: 15 }));

        let r = bitfield.try_set(500_usize);
        assert_eq!(r, None);

        let bits: Vec<u8> = vec![];
        let mut bitfield = Bitfield::from(bits);
        let r = bitfield.try_set(500_usize);
        assert_eq!(r, None);
    }

    #[test]
    fn can_clear_a_bit() {
        let bits: Vec<u8> = vec![0b1000_0001];
        let mut bitfield = Bitfield::from(bits);

        bitfield.clear(0_usize);
        assert_eq!(bitfield.inner[0], 0b0000_0001);

        bitfield.clear(7_usize);
        assert_eq!(bitfield.inner[0], 0b0000_0000);

        let bits: Vec<u8> = vec![];
        let mut bitfield = Bitfield::from(bits);
        let r = bitfield.try_set(500_usize);
        assert_eq!(r, None);
    }

    #[test]
    fn can_iter_a_bit() {
        let bits: Vec<u8> = vec![0b1000_0101, 0b0111_0001];
        let mut bitfield = Bitfield::from(bits);

        assert_eq!(Some(BitItem { bit: 1, index: 0 }), bitfield.next());
        assert_eq!(Some(BitItem { bit: 0, index: 1 }), bitfield.next());
        assert_eq!(Some(BitItem { bit: 0, index: 2 }), bitfield.next());
        assert_eq!(Some(BitItem { bit: 0, index: 3 }), bitfield.next());
        assert_eq!(Some(BitItem { bit: 0, index: 4 }), bitfield.next());
        assert_eq!(Some(BitItem { bit: 1, index: 5 }), bitfield.next());
        assert_eq!(Some(BitItem { bit: 0, index: 6 }), bitfield.next());
        assert_eq!(Some(BitItem { bit: 1, index: 7 }), bitfield.next());

        assert_eq!(Some(BitItem { bit: 0, index: 8 }), bitfield.next());
        assert_eq!(Some(BitItem { bit: 1, index: 9 }), bitfield.next());
        assert_eq!(Some(BitItem { bit: 1, index: 10 }), bitfield.next());
        assert_eq!(Some(BitItem { bit: 1, index: 11 }), bitfield.next());
        assert_eq!(Some(BitItem { bit: 0, index: 12 }), bitfield.next());
        assert_eq!(Some(BitItem { bit: 0, index: 13 }), bitfield.next());
        assert_eq!(Some(BitItem { bit: 0, index: 14 }), bitfield.next());
        assert_eq!(Some(BitItem { bit: 1, index: 15 }), bitfield.next());

        assert_eq!(None, bitfield.next());
    }

    #[test]
    fn receive_lazy_bitfield() {
        let bitfield = Bitfield::from(vec![0b0000_1111, 0b1111_1111]);
        let mut empty_bitfield = Bitfield::default();

        assert_eq!(empty_bitfield.inner.len(), 0_usize);

        let inner = vec![0_u8; bitfield.len_bytes()];

        empty_bitfield = Bitfield::from(inner);

        assert_eq!(empty_bitfield.len_bytes(), 2_usize);

        empty_bitfield.try_set(0_usize);

        assert_eq!(
            empty_bitfield.get(0_usize).unwrap(),
            BitItem { index: 0, bit: 1 }
        );

        assert_eq!(
            empty_bitfield.get(1_usize).unwrap(),
            BitItem { index: 1, bit: 0 }
        );

        empty_bitfield.try_set(1_usize);

        assert_eq!(
            empty_bitfield.get(1_usize).unwrap(),
            BitItem { index: 1, bit: 1 }
        );

        empty_bitfield.try_set(500_usize);

        assert_eq!(
            empty_bitfield.get(0_usize).unwrap(),
            BitItem { index: 0, bit: 1 }
        );
    }
}
