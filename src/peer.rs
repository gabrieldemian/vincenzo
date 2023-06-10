use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use crate::{bitfield::Bitfield, tcp_wire::lib::BlockInfo};

#[derive(Debug, Clone)]
pub struct Peer {
    /// if this client is choking this peer
    pub am_choking: bool,
    /// if this client is interested in this peer
    pub am_interested: bool,
    /// if this peer is choking the client
    pub peer_choking: bool,
    /// if this peer is interested in the client
    pub peer_interested: bool,
    /// a `Bitfield` with pieces that this peer
    /// has, and hasn't, containing 0s and 1s
    pub pieces: Bitfield,
    /// TCP addr that this peer is listening on
    pub addr: SocketAddr,
    pub id: Option<[u8; 20]>,
    /// Requests that we'll send to this peer,
    /// once he unchoke us
    pub pending_requests: Vec<BlockInfo>,
}

impl Default for Peer {
    fn default() -> Self {
        Self {
            // connections start out choked and uninterested,
            // from both sides
            am_choking: true,
            am_interested: false,
            peer_choking: true,
            peer_interested: false,
            pieces: Bitfield::default(),
            addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0),
            pending_requests: vec![],
            id: None,
        }
    }
}

// create a Peer from a `SocketAddr`. Used after
// an announce request with a tracker
impl From<SocketAddr> for Peer {
    fn from(addr: SocketAddr) -> Self {
        let mut s: Self = Self::default();
        s.addr = addr;
        s
    }
}

impl Peer {
    fn new() -> Self {
        Self::default()
    }
}
