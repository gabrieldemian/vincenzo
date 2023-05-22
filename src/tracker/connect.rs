use serde::{Deserialize, Serialize};

use crate::error::Error;

#[derive(Debug, Serialize, Deserialize)]
pub struct Request {
    pub protocol_id: u64,
    pub action: u32,
    pub transaction_id: u32,
}

impl Request {
    pub(crate) const LENGTH: usize = 16;
    const MAGIC: u64 = 0x0417_2710_1980;

    pub fn new() -> Self {
        Self {
            protocol_id: u64::to_be(Self::MAGIC),
            action: 0,
            transaction_id: rand::random::<u32>(),
        }
    }

    pub fn serialize(&self) -> Result<Vec<u8>, Error> {
        bincode::serialize(&self).map_err(Error::Bincode)
    }

    pub fn deserialize(buf: &[u8]) -> Result<Self, Error> {
        bincode::deserialize(buf).map_err(Error::Bincode)
    }
}

#[derive(Debug, Serialize, Deserialize)]
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

    pub fn deserialize(buf: &[u8]) -> Result<Self, Error> {
        // if buf.len() != Self::LENGTH {
        //     return Err(Error::TrackerResponse);
        // }
        bincode::deserialize(buf).map_err(Error::Bincode)
    }

    pub fn serialize(&self) -> Result<Vec<u8>, Error> {
        bincode::serialize(&self).map_err(Error::Bincode)
    }
}
