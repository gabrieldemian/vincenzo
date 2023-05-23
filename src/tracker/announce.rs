use rand::Rng;
use serde::{Deserialize, Serialize};

use super::action::Action;

#[derive(Debug, Serialize, Deserialize)]
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
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Response {
    pub action: u32,
    pub transaction_id: u32,
    pub interval: u32,
    pub leechers: u32,
    pub seeders: u32,
}

impl Response {
    pub(crate) const LENGTH: usize = 20;
}
