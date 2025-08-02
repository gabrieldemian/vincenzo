use speedy::{BigEndian, Readable, Writable};
use tracing::debug;

use crate::error::Error;

use super::action::Action;

#[derive(Debug, PartialEq, Clone, Readable, Writable)]
pub struct Request {
    pub protocol_id: u64,
    pub action: Action,
    pub transaction_id: u32,
}

impl Default for Request {
    fn default() -> Self {
        Self::new()
    }
}

impl Request {
    pub(crate) const LENGTH: usize = 16;
    const MAGIC: u64 = 0x41727101980;

    pub fn new() -> Self {
        Self {
            protocol_id: Self::MAGIC,
            action: Action::Connect,
            transaction_id: rand::random::<u32>(),
        }
    }

    pub fn serialize(&self) -> [u8; 16] {
        debug!("sending connect request {self:#?}");
        let mut buf = [0u8; 16];
        buf[..8].copy_from_slice(&Self::MAGIC.to_be_bytes());
        buf[8..12].copy_from_slice(&(self.action as u32).to_be_bytes());
        buf[12..16].copy_from_slice(&self.transaction_id.to_be_bytes());
        buf
    }

    pub fn deserialize(buf: &[u8]) -> Result<(Self, &[u8]), Error> {
        if buf.len() != Self::LENGTH {
            return Err(Error::TrackerResponse);
        }

        let req = Self::read_from_buffer_with_ctx(BigEndian {}, buf)
            .map_err(Error::SpeedyError)?;

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

    pub fn deserialize(buf: &[u8]) -> Result<(Self, &[u8]), Error> {
        if buf.len() != Self::LENGTH {
            return Err(Error::TrackerResponse);
        }

        let action = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]);
        let transaction_id =
            u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
        let connection_id = u64::from_be_bytes([
            buf[8], buf[9], buf[10], buf[11], buf[12], buf[13], buf[14],
            buf[15],
        ]);

        Ok((
            Self { action, transaction_id, connection_id },
            &buf[Self::LENGTH..],
        ))
    }

    pub fn serialize(&self) -> Vec<u8> {
        self.write_to_vec_with_ctx(BigEndian {}).unwrap()
    }
}
