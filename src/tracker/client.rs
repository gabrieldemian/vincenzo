use std::{
    fmt::Debug,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs, UdpSocket},
    time::Duration,
};

use crate::error::Error;

use super::{announce, connect};

#[derive(Debug)]
pub struct Client {
    pub peer_id: [u8; 20],
    pub tracker_addr: SocketAddr,
    /// UDP Socket of the `tracker_addr`
    pub sock: UdpSocket,
    pub connection_id: Option<u64>,
}

impl Client {
    const ANNOUNCE_RES_BUF_LEN: usize = 8192;
    /// Bind UDP socket and send a connect handshake,
    /// to one of the trackers.
    pub fn connect<A: ToSocketAddrs + Debug>(trackers: Vec<A>) -> Result<Self, Error> {
        for tracker in trackers {
            let addrs = tracker
                .to_socket_addrs()
                .map_err(Error::TrackerSocketAddrs)?;

            for tracker_addr in addrs {
                println!("addr {:#?}", tracker_addr);
                let sock = match Self::new_udp_socket(tracker_addr) {
                    Ok(sock) => sock,
                    Err(e) => {
                        println!("{:#?}", e);
                        continue;
                    }
                };
                let mut client = Client {
                    peer_id: rand::random(),
                    tracker_addr,
                    sock,
                    connection_id: None,
                };
                if client.connect_exchange().is_ok() {
                    println!("connected with tracker ip {tracker_addr}");
                    println!("it has this DNS {:#?}", tracker);
                    return Ok(client);
                }
            }
        }
        Err(Error::TrackerNoHosts)
    }

    /// Create an UDP Socket for the given tracker address
    pub fn new_udp_socket(addr: SocketAddr) -> Result<UdpSocket, Error> {
        let sock = match addr {
            SocketAddr::V4(_) => UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)),
            SocketAddr::V6(_) => UdpSocket::bind((Ipv6Addr::UNSPECIFIED, 0)),
        }
        .expect("Failed to bind udp socket");
        sock.connect(addr).expect("Failed to connect to udp socket");
        sock.set_read_timeout(Some(Duration::new(1, 0)))
            .expect("Failed to set a read timeout to udp socket");

        Ok(sock)
    }

    /// Connect is the first step in getting the file
    fn connect_exchange(&mut self) -> Result<(), Error> {
        let req = connect::Request::new();
        let mut buf = [0u8; connect::Response::LENGTH];
        let mut len: usize = 0;

        // will try to connect up to 3 times
        // breaking if succesfull
        for _ in 0..=2 {
            println!("sending connect...");
            println!("req {:#?}", req);
            self.sock.send(&req.serialize())?;

            match self.sock.recv(&mut buf) {
                Ok(lenn) => {
                    len = lenn;
                    break;
                }
                Err(e) => println!("error receiving {:#?}", e),
            }
        }

        if len == 0 {
            return Err(Error::TrackerResponse);
        }

        let (res, _) = connect::Response::deserialize(&buf)?;

        println!("req {:#?}", req);
        println!("received res {:#?}", res);

        if res.transaction_id != req.transaction_id || res.action != req.action {
            return Err(Error::TrackerResponse);
        }

        self.connection_id.replace(res.connection_id);
        Ok(())
    }

    pub fn announce_exchange(&self, infohash: [u8; 20]) -> Result<Vec<SocketAddr>, Error> {
        let connection_id = match self.connection_id {
            Some(x) => x,
            None => return Err(Error::TrackerNoConnectionId),
        };

        let req = announce::Request::new(
            connection_id,
            infohash,
            self.peer_id,
            self.sock.local_addr()?.port(),
        );

        println!("sending this connection_id {}", connection_id);
        let mut len = 0 as usize;
        let mut res = [0u8; Self::ANNOUNCE_RES_BUF_LEN];

        // will try to connect up to 3 times
        // breaking if succesfull
        for _ in 0..=2 {
            println!("sending announce...");
            self.sock.send(&req.serialize())?;
            match self.sock.recv(&mut res) {
                Ok(lenn) => {
                    len = lenn;
                    break;
                }
                Err(e) => {
                    println!("failed to announce {:#?}", e);
                }
            }
        }

        if len == 0 {
            return Err(Error::TrackerResponse);
        }

        println!("len of res: {len}");
        let res = &res[..len];

        // res is the deserialized struct,
        // payload is a byte array of peers,
        // which are in the form of ips and ports
        let (res, payload) = announce::Response::deserialize(res)?;

        if res.transaction_id != req.transaction_id || res.action != req.action {
            return Err(Error::TrackerResponse);
        }

        println!("got res {:#?}", res);
        println!("got payload {:#?}", payload);

        let peers = Self::parse_compact_peer_list(payload, self.sock.local_addr()?.is_ipv6())?;
        println!("got peers: {:#?}", peers);

        Ok(peers)
    }

    fn parse_compact_peer_list(buf: &[u8], is_ipv6: bool) -> Result<Vec<SocketAddr>, Error> {
        let mut peer_list = Vec::<SocketAddr>::new();

        // in ipv4 the addresses come in packets of 6 bytes,
        // first 4 for ip and 2 for port
        // in ipv6 its 16 bytes for port and 2 for port
        let stride = if is_ipv6 { 18 } else { 6 };

        let chunks = buf.chunks_exact(stride);
        if !chunks.remainder().is_empty() {
            return Err(Error::TrackerCompactPeerList);
        }

        for hostpost in chunks {
            let (ip, port) = hostpost.split_at(stride - 2);
            let ip = if is_ipv6 {
                let octets: [u8; 16] = ip[0..16]
                    .try_into()
                    .expect("iterator guarantees bounds are OK");
                IpAddr::from(std::net::Ipv6Addr::from(octets))
            } else {
                IpAddr::from(std::net::Ipv4Addr::new(ip[0], ip[1], ip[2], ip[3]))
            };

            let port =
                u16::from_be_bytes(port.try_into().expect("iterator guarantees bounds are OK"));

            peer_list.push((ip, port).into());
        }

        Ok(peer_list)
    }
}
