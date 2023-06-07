use bitlab::*;

#[derive(Debug, Clone)]
pub struct Bitfield(Vec<u8>);

impl From<Vec<u8>> for Bitfield {
    fn from(value: Vec<u8>) -> Self {
        Self(value)
    }
}

impl Bitfield {
    fn get_byte<I: Into<usize>>(&self, index: I) -> Option<(u8, usize)> {
        let index: usize = index.into();
        let slice_index = index / 8;
        let bit_index = index % 8;

        if self.0.len() - 1 < slice_index {
            return None;
        }

        Some((self.0[slice_index], bit_index))
    }
    pub fn get<I: Into<usize>>(&self, index: I) -> Option<bool> {
        let (byte, bit_index) = self.get_byte(index)?;
        let r = byte.get_bit(bit_index as u32);
        Some(r.unwrap())
    }

    pub fn set<I: Into<usize>>(&self, index: I) -> Option<u8> {
        let (byte, bit_index) = self.get_byte(index)?;
        let r = byte.set_bit(bit_index as u32);
        Some(r.unwrap())
    }

    pub fn clear<I: Into<usize>>(&self, index: I) -> Option<u8> {
        let (byte, bit_index) = self.get_byte(index)?;
        let r = byte.clear_bit(bit_index as u32);
        Some(r.unwrap())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn can_create_from_vec() {
        let bitfield = Bitfield::from(vec![255 as u8]);
        assert_eq!(bitfield.0, vec![255 as u8]);
    }

    #[test]
    fn can_get_from_bitfield() {
        let bits: Vec<u8> = vec![0b10101010, 0b00011011, 0b00111110];
        let bitfield = Bitfield::from(bits.clone());

        let index_a = bitfield.get(0 as usize);
        let index_b = bitfield.get(3 as usize);
        let index_c = bitfield.get(6 as usize);
        let index_d = bitfield.get(7 as usize);
        assert_eq!(index_a, Some(true));
        assert_eq!(index_b, Some(false));
        assert_eq!(index_c, Some(true));
        assert_eq!(index_d, Some(false));

        let index_a = bitfield.get(8 as usize);
        let index_b = bitfield.get(9 as usize);
        let index_c = bitfield.get(10 as usize);
        let index_d = bitfield.get(11 as usize);
        let index_e = bitfield.get(12 as usize);
        assert_eq!(index_a, Some(false));
        assert_eq!(index_b, Some(false));
        assert_eq!(index_c, Some(false));
        assert_eq!(index_d, Some(true));
        assert_eq!(index_e, Some(true));
        assert_eq!(index_c, Some(false));
    }

    #[test]
    fn can_fail_get_bitfield() {
        let bits: Vec<u8> = vec![0b10101010];
        let bitfield = Bitfield::from(bits.clone());

        let index_a = bitfield.get(8 as usize);
        assert_eq!(index_a, None);
    }

    #[test]
    fn can_set_a_bit() {
        let bits: Vec<u8> = vec![0b0000_0101];
        let bitfield = Bitfield::from(bits.clone());

        let index = bitfield.set(0 as usize);
        assert_eq!(index, Some(0b1000_0101));
    }

    #[test]
    fn can_clear_a_bit() {
        let bits: Vec<u8> = vec![0b1000_0101];
        let bitfield = Bitfield::from(bits.clone());

        let index = bitfield.clear(7 as usize);
        assert_eq!(index, Some(0b1000_0100));
    }
}
