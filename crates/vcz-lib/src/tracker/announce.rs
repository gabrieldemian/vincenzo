use rand::Rng;
use rkyv::{
    Archive, Deserialize, Serialize, api::high::to_bytes_with_alloc,
    ser::allocator::Arena, util::AlignedVec,
};

use crate::{
    config::Config,
    error::Error,
    peer::PeerId,
    torrent::{InfoHash, Stats},
};

use super::{action::Action, event::Event};

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

// impl Default for Request {
//     fn default() -> Self {
//         let config = Config::load().unwrap();
//         Self {
//             connection_id: 0,
//             action: Action::default(),
//             transaction_id: rand::rng().random(),
//             info_hash: InfoHash::default(),
//             peer_id: PeerId::default(),
//             downloaded: 0,
//             left: u64::MAX,
//             uploaded: 0,
//             event: Event::default(),
//             ip_address: 0,
//             key: config.key,
//             num_want: config.max_torrent_peers,
//             port: config.local_peer_port,
//             compact: 1,
//         }
//     }
// }

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
            return Err(Error::TrackerResponse);
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
            return Err(Error::TrackerResponseLength);
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

#[cfg(test)]
mod tests {
    use rkyv::rancor::Error;
    use tokio::time::Instant;

    use super::*;

    // debug mode
    // speedy:    25.356 µs
    // rkyv high: 58.198 µs
    // rkyv low:   3.461 µs
    // #[ignore]
    // #[test]
    // fn serialize() {
    //     let original = Request {
    //         connection_id: 123,
    //         downloaded: 321,
    //         ..Default::default()
    //     };
    //
    //     let now = Instant::now();
    //     let rkyv = rkyv::to_bytes::<rkyv::rancor::Error>(&original).unwrap();
    //     let time = Instant::now().duration_since(now);
    //
    //     println!("rkyv high: {time:?}");
    //     assert_eq!(rkyv.len(), Request::LEN);
    //
    //     use rkyv::{
    //         api::high::to_bytes_with_alloc, rancor::*, ser::allocator::Arena,
    //     };
    //
    //     let mut arena = Arena::with_capacity(Request::LEN);
    //     let now = Instant::now();
    //     let rkyv = to_bytes_with_alloc::<_, Error>(&original, arena.acquire())
    //         .unwrap();
    //     let time = Instant::now().duration_since(now);
    //     println!("rkyv low: {time:?}");
    //
    //     assert_eq!(rkyv.len(), Request::LEN);
    // }

    // debug mode
    // speedy:       20.783 µs
    // rkyv safe:    25.393 µs
    // rkyv unsafe: 116 ns
    // #[ignore]
    // #[test]
    // fn deserialize() {
    //     let original = Request {
    //         connection_id: 123,
    //         downloaded: 321,
    //         ..Default::default()
    //     };
    //
    //     let bytes = original.serialize().unwrap();
    //
    //     let now = Instant::now();
    //     let _archived = rkyv::access::<ArchivedRequest, Error>(&bytes).unwrap();
    //     let time = Instant::now().duration_since(now);
    //     println!("rkyv safe: {time:?}");
    //
    //     let now = Instant::now();
    //     let archived =
    //         unsafe { rkyv::access_unchecked::<ArchivedRequest>(&bytes) };
    //     let time = Instant::now().duration_since(now);
    //     println!("rkyv unsafe: {time:?}");
    //
    //     assert_eq!(archived, &original);
    // }
}
