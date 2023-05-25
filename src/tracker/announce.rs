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

    fn deserialize(buf: &[u8]) -> Result<(Self, &[u8]), Error> {
        if buf.len() != Self::LENGTH {
            return Err(Error::TrackerResponseLength);
        }

        Ok((
            Request {
                connection_id: u64::from_be_bytes(
                    buf[0..8]
                        .try_into()
                        .expect("buf size is at least Request::LENGTH"),
                ),
                action: u32::from_be_bytes(
                    buf[8..12]
                        .try_into()
                        .expect("buf size is at least Request::LENGTH"),
                ),
                transaction_id: u32::from_be_bytes(
                    buf[12..16]
                        .try_into()
                        .expect("buf size is at least Request::LENGTH"),
                ),
                infohash: buf[16..36]
                    .try_into()
                    .expect("buf size is at least Request::LENGTH"),
                peer_id: buf[36..56]
                    .try_into()
                    .expect("buf size is at least Request::LENGTH"),
                downloaded: u64::from_be_bytes(
                    buf[56..64]
                        .try_into()
                        .expect("buf size is at least Request::LENGTH"),
                ),
                left: u64::from_be_bytes(
                    buf[64..72]
                        .try_into()
                        .expect("buf size is at least Request::LENGTH"),
                ),
                uploaded: u64::from_be_bytes(
                    buf[72..80]
                        .try_into()
                        .expect("buf size is at least Request::LENGTH"),
                ),
                event: u64::from_be_bytes(
                    buf[80..88]
                        .try_into()
                        .expect("buf size is at least Request::LENGTH"),
                ),
                ip_address: u32::from_be_bytes(
                    buf[88..92]
                        .try_into()
                        .expect("buf size is at least Request::LENGTH"),
                ),
                num_want: u32::from_be_bytes(
                    buf[92..96]
                        .try_into()
                        .expect("buf size is at least Request::LENGTH"),
                ),
                port: u16::from_be_bytes(
                    buf[96..98]
                        .try_into()
                        .expect("buf size is at least Request::LENGTH"),
                ),
            },
            &buf[Self::LENGTH..],
        ))
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
    }
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

    pub fn deserialize(buf: &[u8]) -> Result<(Self, &[u8]), Error> {
        if buf.len() < Response::LENGTH {
            return Err(Error::TrackerResponseLength);
        }

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
