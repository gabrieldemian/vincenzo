mod codec;

// re-exports
pub use codec::*;

use crate::{error::Error, extensions::ExtMsg};
use int_enum::IntEnum;
use rkyv::{
    Archive, Deserialize, Serialize, api::high::to_bytes_with_alloc,
    ser::allocator::Arena, util::AlignedVec,
};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

#[repr(u8)]
#[derive(
    PartialEq, Debug, Serialize, Deserialize, Archive, Clone, Copy, IntEnum,
)]
#[rkyv(compare(PartialEq), derive(Debug))]
pub enum HolepunchMsgType {
    /// Send connect messages to both the initiating peer and target peer
    Rendezvous = 0,

    /// Initiate a uTP connection to designated endpoint
    Connect = 1,

    /// Rendezvous operation cannot be completed
    Error = 2,
}

#[repr(u8)]
#[derive(
    PartialEq,
    Debug,
    Serialize,
    Deserialize,
    Archive,
    Clone,
    Copy,
    Default,
    IntEnum,
)]
#[rkyv(compare(PartialEq), derive(Debug))]
pub enum HolepunchErrorCodes {
    /// No error
    #[default]
    NoError = 0,

    /// The target endpoint is invalid.
    NoSuchPeer = 1,

    /// The relaying peer is not connected to the target peer.
    NotConnected = 2,

    /// The target peer does not support the holepunch extension.
    NoSupport = 3,

    /// The target endpoint belongs to the relaying peer.
    NoSelf = 4,
}

#[repr(u8)]
#[derive(
    PartialEq, Debug, Serialize, Deserialize, Archive, Clone, Copy, IntEnum,
)]
#[rkyv(compare(PartialEq), derive(Debug))]
pub enum HolepunchAddrType {
    Ipv4 = 0,
    Ipv6 = 1,
}

#[repr(u8)]
#[derive(PartialEq, Debug, Serialize, Deserialize, Archive, Clone, Copy)]
#[rkyv(compare(PartialEq), derive(Debug))]
pub enum HolepunchAddr {
    Ipv4(u32),
    Ipv6(u128),
}

impl From<HolepunchAddr> for IpAddr {
    fn from(value: HolepunchAddr) -> Self {
        match value {
            HolepunchAddr::Ipv6(ip) => IpAddr::V6(Ipv6Addr::from_bits(ip)),
            HolepunchAddr::Ipv4(ip) => IpAddr::V4(Ipv4Addr::from_bits(ip)),
        }
    }
}

impl From<SocketAddr> for HolepunchAddr {
    fn from(value: SocketAddr) -> Self {
        match value {
            SocketAddr::V4(ip) => {
                HolepunchAddr::Ipv4(u32::from_be_bytes(*ip.ip().as_octets()))
            }
            SocketAddr::V6(ip) => {
                HolepunchAddr::Ipv6(u128::from_be_bytes(*ip.ip().as_octets()))
            }
        }
    }
}

#[derive(PartialEq, Debug, Serialize, Deserialize, Archive, Clone)]
#[rkyv(compare(PartialEq), derive(Debug))]
pub struct Holepunch {
    pub msg_type: HolepunchMsgType, // 1 byte

    pub addr_type: HolepunchAddrType, // 1 byte

    /// big-endian
    pub addr: HolepunchAddr, // 16 bytes

    /// big-endian
    pub port: u16, // 2 bytes

    /// big-endian, 0 if non error messages.
    pub err_code: HolepunchErrorCodes, // 1 byte
}

impl ExtMsg for Holepunch {
    const ID: u8 = 4;
}

impl Holepunch {
    pub const LEN: usize = 21;

    pub fn serialize(&self) -> Result<AlignedVec, Error> {
        let mut arena = Arena::with_capacity(Self::LEN);
        Ok(to_bytes_with_alloc::<_, rkyv::rancor::Error>(
            self,
            arena.acquire(),
        )?)
    }

    pub fn deserialize(bytes: &[u8]) -> Result<&ArchivedHolepunch, Error> {
        if bytes.len() != Self::LEN {
            return Err(Error::TrackerResponse);
        }
        Ok(unsafe { rkyv::access_unchecked::<ArchivedHolepunch>(bytes) })
    }

    pub fn error(self, err_code: HolepunchErrorCodes) -> Self {
        Self {
            msg_type: self.msg_type,
            addr_type: self.addr_type,
            addr: self.addr,
            port: self.port,
            err_code,
        }
    }

    pub fn connect(addr: HolepunchAddr, port: u16) -> Self {
        Self {
            msg_type: HolepunchMsgType::Connect,
            addr_type: if let HolepunchAddr::Ipv4(_) = addr {
                HolepunchAddrType::Ipv4
            } else {
                HolepunchAddrType::Ipv6
            },
            addr,
            port,
            err_code: HolepunchErrorCodes::NoError,
        }
    }

    /// The initiating peer sends a rendezvous message to the relaying peer,
    /// containing the endpoint (IP address and port) of the target peer
    pub fn rendezvous(addr: SocketAddr) -> Self {
        let ip = addr.ip();

        match addr {
            SocketAddr::V6(addr) => {
                let mut buf = [0u8; 16];
                let ip_bytes = ip.as_octets();
                buf.copy_from_slice(ip_bytes);
                Holepunch {
                    msg_type: HolepunchMsgType::Rendezvous,
                    addr_type: HolepunchAddrType::Ipv6,
                    addr: HolepunchAddr::Ipv6(u128::from_be_bytes(buf)),
                    port: addr.port(),
                    err_code: HolepunchErrorCodes::NoError,
                }
            }
            SocketAddr::V4(addr) => {
                let ip_bytes = ip.as_octets();
                let mut buf = [0u8; 4];
                buf.copy_from_slice(ip_bytes);
                Holepunch {
                    msg_type: HolepunchMsgType::Rendezvous,
                    addr_type: HolepunchAddrType::Ipv4,
                    addr: HolepunchAddr::Ipv4(u32::from_be_bytes(buf)),
                    port: addr.port(),
                    err_code: HolepunchErrorCodes::NoError,
                }
            }
        }
    }
}

// impl TryFrom<Vec<u8>> for Holepunch {
//     type Error = Error;
//     fn try_from(value: Vec<u8>) -> Result<Self, Self::Error> {
//         Ok(Self::read_from_buffer_with_ctx(BigEndian {}, &value)?)
//     }
// }
//
// impl TryInto<Vec<u8>> for Holepunch {
//     type Error = Error;
//     fn try_into(self) -> Result<Vec<u8>, Self::Error> {
//         Ok(self.write_to_vec_with_ctx(BigEndian {})?)
//     }
// }

// 1. The initiating peer (client) sends a rendezvous message to the relaying
//    peer, containing the endpoint (IP address and port) of the target peer
//    that the client wants to connect to.
//
// 2. If the relaying peer is connected to the target peer, and the target peer
//    supports this extension, the relaying peer sends a connect message to both
//    the initiating peer and the target peer, each containing the endpoint of
//    the other.
//
// 3. Upon receiving the connect message, each peer initiates a uTP [2]
//    connection to the other peer.

// You connect to RelayPeer (gets NAT mapping 203.0.113.5:55679)
//
// You tell RelayPeer: "Help me connect to TargetPeer"
//
// RelayPeer tells TargetPeer: "Connect to 203.0.113.5:55679"
//
// RelayPeer tells You: "Connect to 198.51.100.10:33445"
//
// Both peers simultaneously connect to each other's specified endpoints
