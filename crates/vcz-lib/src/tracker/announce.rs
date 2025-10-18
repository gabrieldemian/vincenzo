use super::{action::Action, event::Event};
use crate::{
    error::Error,
    peer::PeerId,
    torrent::{InfoHash, Stats},
};
use rkyv::{
    Archive, Deserialize, Serialize, api::high::to_bytes_with_alloc,
    ser::allocator::Arena, util::AlignedVec,
};

#[derive(Debug, PartialEq, Archive, Serialize, Deserialize)]
#[rkyv(compare(PartialEq), derive(Debug))]
pub struct Request {
    pub connection_id: u64,
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

impl Request {
    pub const LEN: usize = 99;

    /// Around 5µs in release mode.
    pub fn serialize(&self) -> Result<AlignedVec, Error> {
        let mut arena = Arena::with_capacity(Self::LEN);
        Ok(to_bytes_with_alloc::<_, rkyv::rancor::Error>(
            self,
            arena.acquire(),
        )?)
    }

    /// Around 20ns in release mode.
    pub fn deserialize(bytes: &[u8]) -> Result<&ArchivedRequest, Error> {
        if bytes.len() != Self::LEN {
            return Err(Error::ResponseLen);
        }
        Ok(unsafe { rkyv::access_unchecked::<ArchivedRequest>(bytes) })
    }
}

#[derive(Debug, PartialEq, Serialize, Deserialize, Archive)]
#[rkyv(compare(PartialEq), derive(Debug))]
pub struct Response {
    pub action: Action,
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
    pub const MIN_LEN: usize = 20;

    /// Around 237ns in release mode.
    pub fn serialize(&self) -> Result<AlignedVec, Error> {
        let mut arena = Arena::with_capacity(Self::MIN_LEN);
        Ok(to_bytes_with_alloc::<_, rkyv::rancor::Error>(
            self,
            arena.acquire(),
        )?)
    }

    pub fn deserialize(
        bytes: &[u8],
    ) -> Result<(&ArchivedResponse, &[u8]), Error> {
        if bytes.len() < Response::MIN_LEN {
            return Err(Error::ResponseLen);
        }
        Ok((
            unsafe {
                rkyv::access_unchecked::<ArchivedResponse>(
                    &bytes[..Self::MIN_LEN],
                )
            },
            &bytes[Self::MIN_LEN..],
        ))
    }
}
