use std::ops::{Deref, DerefMut};

use bitlab::*;

#[derive(Debug, Clone)]
pub struct Bitfield {
    pub inner: Vec<u8>,
    curr: usize,
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
    type Item = u8;
    fn next(&mut self) -> Option<Self::Item> {
        self.curr += 1;
        let bit = self.get(self.curr);
        bit
    }
}

impl Deref for Bitfield {
    type Target = Vec<u8>;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl DerefMut for Bitfield {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

impl Bitfield {
    /// Get byte and bit_index that correspond to the provided bit
    fn get_byte<I: Into<usize>>(&self, index: I) -> Option<(u8, usize, usize)> {
        // index of the bit
        let index: usize = index.into();
        // index of the slice, where the bit lives in
        let slice_index = index / 8;
        // position of the bit/index under slice_index
        let bit_index = index % 8;

        if self.inner.len() - 1 < slice_index {
            return None;
        }

        // byte where `index` lives in
        let byte = self.inner[slice_index];

        Some((byte, slice_index, bit_index))
    }

    /// Return true if the provided index is 1. False otherwise.
    pub fn has<I: Into<usize>>(&self, index: I) -> Option<bool> {
        let (byte, _, bit_index) = self.get_byte(index)?;
        let r = byte.get_bit(bit_index as u32);
        Some(r.unwrap())
    }

    /// Get a bit, 0 or 1
    pub fn get<I: Into<usize>>(&self, index: I) -> Option<u8> {
        let (byte, _, bit_index) = self.get_byte(index)?;
        let r = byte.get_bit(bit_index as u32).unwrap();

        if r {
            return Some(1 as u8);
        } else {
            return Some(0 as u8);
        }
    }

    /// Set a bit, turn a 0 into a 1
    pub fn set<I: Into<usize>>(&mut self, index: I) -> Option<u8> {
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
    /// Returns the lenght of bits
    pub fn len(&self) -> usize {
        self.inner.len() * 8 as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn can_create_from_vec() {
        let bitfield = Bitfield::from(vec![255 as u8]);
        assert_eq!(bitfield.inner, vec![255 as u8]);
    }

    #[test]
    fn can_has_from_bitfield() {
        let bits: Vec<u8> = vec![0b10101010, 0b00011011, 0b00111110];
        let bitfield = Bitfield::from(bits.clone());

        let index_a = bitfield.has(0 as usize);
        let index_b = bitfield.has(3 as usize);
        let index_c = bitfield.has(6 as usize);
        let index_d = bitfield.has(7 as usize);
        assert_eq!(index_a, Some(true));
        assert_eq!(index_b, Some(false));
        assert_eq!(index_c, Some(true));
        assert_eq!(index_d, Some(false));

        let index_a = bitfield.has(8 as usize);
        let index_b = bitfield.has(9 as usize);
        let index_c = bitfield.has(10 as usize);
        let index_d = bitfield.has(11 as usize);
        let index_e = bitfield.has(12 as usize);
        assert_eq!(index_a, Some(false));
        assert_eq!(index_b, Some(false));
        assert_eq!(index_c, Some(false));
        assert_eq!(index_d, Some(true));
        assert_eq!(index_e, Some(true));
        assert_eq!(index_c, Some(false));
    }

    #[test]
    fn can_get_from_bitfield() {
        let bits: Vec<u8> = vec![0b10101010, 0b00011011, 0b00111110];
        let bitfield = Bitfield::from(bits.clone());

        let index_a = bitfield.get(0 as usize);
        let index_b = bitfield.get(3 as usize);
        let index_c = bitfield.get(6 as usize);
        let index_d = bitfield.get(7 as usize);
        assert_eq!(index_a, Some(1));
        assert_eq!(index_b, Some(0));
        assert_eq!(index_c, Some(1));
        assert_eq!(index_d, Some(0));

        let index_a = bitfield.get(8 as usize);
        let index_b = bitfield.get(9 as usize);
        let index_c = bitfield.get(10 as usize);
        let index_d = bitfield.get(11 as usize);
        let index_e = bitfield.get(12 as usize);
        assert_eq!(index_a, Some(0));
        assert_eq!(index_b, Some(0));
        assert_eq!(index_c, Some(0));
        assert_eq!(index_d, Some(1));
        assert_eq!(index_e, Some(1));
        assert_eq!(index_c, Some(0));
    }

    #[test]
    fn can_fail_get_bitfield() {
        let bits: Vec<u8> = vec![0b10101010];
        let bitfield = Bitfield::from(bits);

        let index_a = bitfield.has(8 as usize);
        assert_eq!(index_a, None);
    }

    #[test]
    fn can_set_a_bit() {
        let bits: Vec<u8> = vec![0b0000_0000];
        let mut bitfield = Bitfield::from(bits);

        bitfield.set(0 as usize);
        assert_eq!(bitfield.inner[0], 0b1000_0000);

        bitfield.set(7 as usize);
        assert_eq!(bitfield.inner[0], 0b1000_0001);
    }

    #[test]
    fn can_clear_a_bit() {
        let bits: Vec<u8> = vec![0b1000_0001];
        let mut bitfield = Bitfield::from(bits);

        bitfield.clear(0 as usize);
        assert_eq!(bitfield.inner[0], 0b0000_0001);

        bitfield.clear(7 as usize);
        assert_eq!(bitfield.inner[0], 0b0000_0000);
    }

    #[test]
    fn can_iter_a_bit() {
        let bits: Vec<u8> = vec![0b1000_0101];
        let mut bitfield = Bitfield::from(bits).into_iter();

        assert_eq!(Some(0), bitfield.next());
        assert_eq!(Some(0), bitfield.next());
        assert_eq!(Some(0), bitfield.next());
        assert_eq!(Some(0), bitfield.next());
        assert_eq!(Some(1), bitfield.next());
        assert_eq!(Some(0), bitfield.next());
        assert_eq!(Some(1), bitfield.next());
        assert_eq!(None, bitfield.next());
    }
}
