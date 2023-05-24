use rand::Rng;

use crate::error::Error;

use super::action::Action;

#[derive(Debug, PartialEq, Clone, Copy)]
pub struct Request {
    pub protocol_id: u64,
    pub action: u32,
    pub transaction_id: u32,
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
        let mut msg = Vec::new();

        msg.extend_from_slice(&self.protocol_id.to_be_bytes());
        msg.extend_from_slice(&self.action.to_be_bytes());
        msg.extend_from_slice(&self.transaction_id.to_be_bytes());

        msg
    }

    pub fn deserialize(buf: &[u8]) -> Result<(Self, &[u8]), Error> {
        // bincode::deserialize(buf).map_err(Error::Bincode)
        if buf.len() != Self::LENGTH {
            return Err(Error::TrackerResponse);
        }

        Ok((
            Request {
                protocol_id: u64::from_be_bytes(
                    buf[0..8]
                        .try_into()
                        .expect("incoming type guarantees bounds are OK"),
                ),
                action: u32::from_be_bytes(
                    buf[8..12]
                        .try_into()
                        .expect("incoming type guarantees bounds are OK"),
                ),
                transaction_id: u32::from_be_bytes(
                    buf[12..16]
                        .try_into()
                        .expect("incoming type guarantees bounds are OK"),
                ),
            },
            &buf[Self::LENGTH..],
        ))
    }
}

#[derive(Debug, PartialEq)]
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
        Ok((
            Self {
                action: u32::from_be_bytes(
                    buf[0..4]
                        .try_into()
                        .expect("bounds are checked manually above"),
                ),
                transaction_id: u32::from_be_bytes(
                    buf[4..8]
                        .try_into()
                        .expect("bounds are checked manually above"),
                ),
                connection_id: u64::from_be_bytes(
                    buf[8..16]
                        .try_into()
                        .expect("bounds are checked manually above"),
                ),
            },
            &buf[Self::LENGTH..],
        ))
    }

    pub fn serialize(&self) -> Vec<u8> {
        // bincode::serialize(&self).map_err(Error::Bincode)
        let mut msg = Vec::new();

        msg.extend_from_slice(&self.action.to_be_bytes());
        msg.extend_from_slice(&self.transaction_id.to_be_bytes());
        msg.extend_from_slice(&self.connection_id.to_be_bytes());

        msg
    }
}
