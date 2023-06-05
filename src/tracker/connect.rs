use rand::Rng;
use speedy::{BigEndian, Readable, Writable};

use crate::error::Error;

use super::action::Action;

#[derive(Debug, PartialEq, Clone, Readable, Writable)]
pub struct Request {
    pub protocol_id: u64,
    pub action: u32,
    pub transaction_id: u32,
}

impl Default for Request {
    fn default() -> Self {
        Self::new()
    }
}

impl Request {
    pub(crate) const LENGTH: usize = 16;
    const MAGIC: u64 = 0x0000_0417_2710_1980;

    pub fn new() -> Self {
        Self {
            protocol_id: Self::MAGIC,
            action: Action::Connect.into(),
            transaction_id: rand::thread_rng().gen(),
        }
    }

    pub fn serialize(&self) -> Vec<u8> {
        self.write_to_vec_with_ctx(BigEndian {}).unwrap()
    }

    pub fn deserialize(buf: &[u8]) -> Result<(Self, &[u8]), Error> {
        if buf.len() != Self::LENGTH {
            return Err(Error::TrackerResponse);
        }

        let req = Self::read_from_buffer_with_ctx(BigEndian {}, buf).map_err(Error::SpeedyError)?;

        Ok((req, &buf[Self::LENGTH..]))
    }
}

#[derive(Debug, PartialEq, Readable, Writable)]
pub struct Response {
    pub action: u32,
    pub transaction_id: u32,
    pub connection_id: u64,
}

impl Response {
    pub(crate) const LENGTH: usize = 16;

    pub fn new() -> Self {
        Self {
            action: 0,
            transaction_id: 0,
            connection_id: 0,
        }
    }

    pub fn deserialize(buf: &[u8]) -> Result<(Self, &[u8]), Error> {
        if buf.len() != Self::LENGTH {
            return Err(Error::TrackerResponse);
        }

        let res = Self::read_from_buffer_with_ctx(BigEndian {}, buf).map_err(Error::SpeedyError)?;

        Ok((res, &buf[Self::LENGTH..]))
    }

    pub fn serialize(&self) -> Vec<u8> {
        self.write_to_vec_with_ctx(BigEndian {}).unwrap()
    }
}
