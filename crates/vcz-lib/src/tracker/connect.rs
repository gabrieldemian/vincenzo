use super::action::Action;
use crate::error::Error;
use rkyv::{
    Archive, Deserialize, Serialize, api::high::to_bytes_with_alloc,
    ser::allocator::Arena, util::AlignedVec,
};

#[derive(Debug, PartialEq, Clone, Serialize, Deserialize, Archive)]
#[rkyv(compare(PartialEq), derive(Debug))]
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
    pub const LEN: usize = 16;
    const MAGIC: u64 = 0x41727101980;
    const MAGIC_BUF: [u8; 8] = Self::MAGIC.to_be_bytes();

    pub fn new() -> Self {
        Self {
            protocol_id: Self::MAGIC,
            action: Action::Connect,
            transaction_id: rand::random::<u32>(),
        }
    }

    /// Zero-copy serialize, this takes around `484ns` in release mode while
    /// serializing by hand takes around `1.94µs`, around 4x faster.
    pub fn serialize(&self) -> Result<AlignedVec, Error> {
        let mut arena = Arena::with_capacity(16);
        Ok(to_bytes_with_alloc::<_, rkyv::rancor::Error>(
            self,
            arena.acquire(),
        )?)
    }

    #[cfg(test)]
    fn serialize_hand(&self) -> [u8; 16] {
        let mut buf = [0u8; 16];
        buf[..8].copy_from_slice(&Self::MAGIC.to_be_bytes());
        buf[8..12].copy_from_slice(&(self.action as u32).to_be_bytes());
        buf[12..16].copy_from_slice(&self.transaction_id.to_be_bytes());
        buf
    }

    pub fn deserialize(bytes: &[u8]) -> Result<&ArchivedRequest, Error> {
        if bytes.len() != Self::LEN {
            return Err(Error::ResponseLen);
        }
        if bytes[0..8] != Self::MAGIC_BUF {
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
    pub connection_id: u64,
}

impl Response {
    pub const LEN: usize = 16;

    #[cfg(test)]
    pub(crate) fn hand(buf: &[u8]) -> Result<Self, Error> {
        if buf.len() != Self::LEN {
            return Err(Error::ResponseLen);
        }

        // let action = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]);
        let action: Action =
            u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]])
                .try_into()
                .unwrap();
        let transaction_id =
            u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
        let connection_id = u64::from_be_bytes([
            buf[8], buf[9], buf[10], buf[11], buf[12], buf[13], buf[14],
            buf[15],
        ]);

        Ok(Self { action, transaction_id, connection_id })
    }

    pub fn deserialize(bytes: &[u8]) -> Result<&ArchivedResponse, Error> {
        if bytes.len() != Self::LEN {
            return Err(Error::ResponseLen);
        }
        Ok(unsafe { rkyv::access_unchecked::<ArchivedResponse>(bytes) })
    }

    pub fn serialize(&self) -> Result<AlignedVec, Error> {
        let mut arena = Arena::with_capacity(Self::LEN);
        Ok(to_bytes_with_alloc::<_, rkyv::rancor::Error>(
            self,
            arena.acquire(),
        )?)
    }
}

#[cfg(test)]
mod tests {
    use tokio::time::Instant;

    use super::*;

    #[test]
    fn new_and_default() {
        let n = Request::new();
        assert_eq!(n.action, Action::Connect);
        assert_eq!(n.protocol_id, Request::MAGIC);
        let d = Request::default();
        assert_eq!(d.action, Action::Connect);
        assert_eq!(d.protocol_id, Request::MAGIC);
    }

    #[ignore]
    #[test]
    fn serialize() {
        let n = Request::new();

        let now = Instant::now();
        let b = n.serialize_hand();
        let time = Instant::now().duration_since(now);
        println!("hand: {time:?}");

        let now = Instant::now();
        let c = n.serialize().unwrap();
        let time = Instant::now().duration_since(now);
        println!("rkyv: {time:?}");

        assert_eq!(b, *c);
    }

    #[test]
    #[ignore]
    fn deserialize() {
        let n = Response {
            action: Action::Scrape,
            connection_id: 123,
            transaction_id: 987,
        };
        let b = n.serialize().unwrap();

        let now = Instant::now();
        let _ = Response::hand(&b).unwrap();
        let time = Instant::now().duration_since(now);
        println!("hand: {time:?}");

        let now = Instant::now();
        let d = Response::deserialize(&b).unwrap();
        let time = Instant::now().duration_since(now);
        println!("rkyv: {time:?}");

        assert_eq!(n, *d);
    }
}
