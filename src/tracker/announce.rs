use rand::Rng;

use crate::error::Error;

use super::action::Action;

#[derive(Debug, PartialEq)]
pub struct Request {
    pub connection_id: u64,
    pub action: u32,
    pub transaction_id: u32,
    pub infohash: [u8; 20],
    pub peer_id: [u8; 20],
    pub downloaded: u64,
    pub left: u64,
    pub uploaded: u64,
    pub event: u64,
    pub ip_address: u32,
    pub num_want: u32,
    pub port: u16,
}

impl Request {
    pub(crate) const LENGTH: usize = 98;

    pub fn new(connection_id: u64, infohash: [u8; 20], peer_id: [u8; 20], port: u16) -> Self {
        let mut rng = rand::thread_rng();
        Self {
            connection_id,
            action: Action::Announce.into(),
            transaction_id: rng.gen(),
            infohash,
            peer_id,
            downloaded: 0x0000,
            left: u64::MAX,
            uploaded: 0x0000,
            event: 0x0000,
            ip_address: 0x0000,
            num_want: u32::MAX,
            port,
        }
    }

    pub fn serialize(&self) -> Vec<u8> {
        let mut msg = Vec::new();
        msg.extend_from_slice(&self.connection_id.to_be_bytes());
        msg.extend_from_slice(&self.action.to_be_bytes());
        msg.extend_from_slice(&self.transaction_id.to_be_bytes());
        msg.extend_from_slice(&self.infohash);
        msg.extend_from_slice(&self.peer_id);
        msg.extend_from_slice(&self.downloaded.to_be_bytes());
        msg.extend_from_slice(&self.left.to_be_bytes());
        msg.extend_from_slice(&self.uploaded.to_be_bytes());
        msg.extend_from_slice(&self.event.to_be_bytes());
        msg.extend_from_slice(&self.ip_address.to_be_bytes());
        msg.extend_from_slice(&self.num_want.to_be_bytes());
        msg.extend_from_slice(&self.port.to_be_bytes());
        msg
        // bincode::serialize(&self).unwrap()
    }

    // pub fn deserialize(buf: &[u8]) -> Result<Self, Error> {
    //     bincode::deserialize(buf).map_err(Error::Bincode)
    // }
}

#[derive(Debug, PartialEq)]
pub struct Response {
    pub action: u32,
    pub transaction_id: u32,
    pub interval: u32,
    pub leechers: u32,
    pub seeders: u32,
}

impl Response {
    pub(crate) const LENGTH: usize = 20;

    pub fn new() -> Self {
        Self {
            action: 0,
            transaction_id: 0,
            interval: 0,
            leechers: 0,
            seeders: 0,
        }
    }

    // pub fn serialize(&self) -> Result<Vec<u8>, Error> {
    //     bincode::serialize(&self).map_err(Error::Bincode)
    // }

    pub fn deserialize(buf: &[u8]) -> Result<(Self, &[u8]), Error> {
        Ok((
            Self {
                action: u32::from_be_bytes(
                    buf[0..4]
                        .try_into()
                        .expect("buf size is at least Response::LENGTH"),
                ),
                transaction_id: u32::from_be_bytes(
                    buf[4..8]
                        .try_into()
                        .expect("buf size is at least Response::LENGTH"),
                ),
                interval: u32::from_be_bytes(
                    buf[8..12]
                        .try_into()
                        .expect("buf size is at least Response::LENGTH"),
                ),
                leechers: u32::from_be_bytes(
                    buf[12..16]
                        .try_into()
                        .expect("buf size is at least Response::LENGTH"),
                ),
                seeders: u32::from_be_bytes(
                    buf[16..20]
                        .try_into()
                        .expect("buf size is at least Response::LENGTH"),
                ),
            },
            &buf[Self::LENGTH..],
        ))
    }
}
