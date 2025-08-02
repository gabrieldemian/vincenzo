use rand::Rng;
use speedy::{BigEndian, Readable, Writable};

use crate::{
    config::CONFIG,
    error::Error,
    peer::PeerId,
    torrent::{InfoHash, Stats},
};

use super::{action::Action, event::Event};

#[derive(Debug, PartialEq, Readable, Writable)]
pub struct Request {
    pub connection_id: u64,
    /// The documentation say this is a u32, but the request only works if this
    /// is u64...
    pub action: Action,
    pub transaction_id: u32,
    pub info_hash: InfoHash,
    pub peer_id: PeerId,
    pub downloaded: u64,
    pub left: u64,
    pub uploaded: u64,
    pub event: Event,
    pub ip_address: u32, // 0 default
    pub key: u32,
    pub num_want: u32,
    pub port: u16,
    pub compact: u8,
}

impl Default for Request {
    fn default() -> Self {
        Self {
            connection_id: 0,
            action: Action::default(),
            transaction_id: rand::rng().random(),
            info_hash: InfoHash::default(),
            peer_id: PeerId::default(),
            downloaded: 0,
            left: u64::MAX,
            uploaded: 0,
            event: Event::default(),
            ip_address: 0,
            key: CONFIG.key,
            num_want: CONFIG.max_torrent_peers,
            port: CONFIG.local_peer_port,
            compact: 1,
        }
    }
}

impl Request {
    pub fn from_started(
        connection_id: u64,
        info_hash: InfoHash,
        peer_id: PeerId,
        port: u16,
    ) -> Self {
        Self { connection_id, info_hash, peer_id, port, ..Default::default() }
    }

    pub fn new(
        connection_id: u64,
        info_hash: InfoHash,
        peer_id: PeerId,
        // _ip_address: u32,
        port: u16,
        event: Event,
    ) -> Self {
        Self {
            connection_id,
            info_hash,
            peer_id,
            event,
            port,
            ..Default::default()
        }
    }

    pub fn serialize(&self) -> Vec<u8> {
        self.write_to_vec_with_ctx(BigEndian {}).unwrap()
    }
}

#[derive(Debug, PartialEq, Writable, Readable)]
pub struct Response {
    pub action: u32,
    pub transaction_id: u32,
    pub interval: u32,
    pub leechers: u32,
    pub seeders: u32,
    // binary of peers that will be deserialized on
}

impl From<Response> for Stats {
    fn from(value: Response) -> Self {
        Self {
            interval: value.interval,
            seeders: value.seeders,
            leechers: value.leechers,
        }
    }
}

impl Response {
    pub(crate) const MIN_LEN: usize = 20;

    pub fn deserialize(buf: &[u8]) -> Result<(Self, &[u8]), Error> {
        if buf.len() < Response::MIN_LEN {
            return Err(Error::TrackerResponseLength);
        }

        let res = Self::read_from_buffer_with_ctx(BigEndian {}, buf)?;

        Ok((res, &buf[Self::MIN_LEN..]))
    }
}
