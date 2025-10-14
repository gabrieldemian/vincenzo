//! Mock of an UDP tracker.

use hashbrown::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use tokio::net::UdpSocket;
use vcz_lib::{error::Error, peer::PeerId, torrent::InfoHash, tracker};

pub static DEFAULT_ADDR: SocketAddr =
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 1337);

#[derive(Debug, PartialEq, Eq, PartialOrd, Clone)]
pub(crate) struct PeerInfo {
    pub connection_id: u64,
    pub key: u32,
    pub addr: SocketAddr,
}

pub(crate) struct MockTracker {
    socket: UdpSocket,

    /// Peers that are connected, but could be not announced.
    // socket_addr -> connection_id
    peers: HashMap<SocketAddr, u64>,

    /// Leechers that are both announced and connected.
    leechers: HashMap<PeerId, PeerInfo>,

    /// Seeders that are both announced and connected.
    seeders: HashMap<PeerId, PeerInfo>,
}

impl MockTracker {
    pub async fn new() -> Result<Self, Error> {
        Self::from(DEFAULT_ADDR).await
    }

    pub async fn from(addr: SocketAddr) -> Result<Self, Error> {
        let socket = UdpSocket::bind(addr).await?;
        Ok(Self {
            socket,
            peers: Default::default(),
            leechers: Default::default(),
            seeders: Default::default(),
        })
    }

    pub async fn run(&mut self) -> Result<(), Error> {
        let mut buf = [0u8; 99];
        loop {
            let (len, who) = self.socket.recv_from(&mut buf).await?;
            let _ = self.handle_packet(&buf[..len], who).await;
        }
    }

    pub fn insert_seeder(&mut self, v: (PeerId, PeerInfo)) {
        self.peers.insert(v.1.addr, v.1.connection_id);
        self.seeders.insert(v.0, v.1);
    }

    pub fn insert_leecher(&mut self, v: (PeerId, PeerInfo)) {
        self.peers.insert(v.1.addr, v.1.connection_id);
        self.leechers.insert(v.0, v.1);
    }

    pub async fn handle_packet(
        &mut self,
        buf: &[u8],
        who: SocketAddr,
    ) -> Result<(), Error> {
        let peer_conn = self.peers.get(&who);

        match (peer_conn, buf.len()) {
            // handle new connection
            (None, tracker::connect::Request::LEN) => {
                let peer_conn = self.peers.get(&who).cloned();
                let req = tracker::connect::Request::deserialize(buf)?;
                let connection_id = peer_conn.unwrap_or(rand::random());

                let res = tracker::connect::Response {
                    action: req.action.to_native(),
                    transaction_id: req.transaction_id.to_native(),
                    connection_id,
                };

                self.peers.insert(who, connection_id);
                self.socket.send_to(&res.serialize()?, who).await?;
                Ok(())
            }

            // handle announces
            (Some(&conn), tracker::announce::Request::LEN) => {
                let req = tracker::announce::Request::deserialize(buf)?;

                if req.connection_id != conn {
                    return Err(Error::TrackerResponse);
                }

                let (id, info) = (
                    PeerId(req.peer_id.0),
                    PeerInfo {
                        connection_id: conn,
                        key: req.key.to_native(),
                        addr: {
                            // some trackers ignore the `ip_addr` from the req
                            // and just use the addr
                            // of the socket.
                            // let v = req.ip_address.to_native().to_be_bytes();
                            let v = who.ip();
                            let v = v.as_octets();

                            SocketAddr::V4(SocketAddrV4::new(
                                Ipv4Addr::new(v[0], v[1], v[2], v[3]),
                                req.port.to_native(),
                            ))
                        },
                    },
                );

                if req.left == 0 {
                    self.seeders.insert(id, info);
                } else {
                    self.leechers.insert(id, info);
                }

                let res = tracker::announce::Response {
                    action: req.action.to_native(),
                    transaction_id: req.transaction_id.into(),
                    interval: 123,
                    seeders: self.seeders.len() as u32,
                    leechers: self.leechers.len() as u32,
                };

                let mut peers: Vec<u8> =
                    Vec::with_capacity(req.num_want.to_native() as usize);

                let to_take = req.num_want.to_native() as usize / 2;

                for peer in self
                    .seeders
                    .values()
                    .filter(|v| v.connection_id != conn)
                    .take(to_take)
                    .chain(
                        self.leechers
                            .values()
                            .filter(|v| v.connection_id != conn)
                            .take(to_take),
                    )
                {
                    peers.extend_from_slice(peer.addr.ip().as_octets());
                    let p = peer.addr.port();
                    peers.extend_from_slice(&p.to_be_bytes());
                }

                let ser = res.serialize()?;
                let mut buf = Vec::with_capacity(ser.len());

                buf.extend(ser.to_vec());
                buf.extend(peers);

                self.socket.send_to(&buf, who).await?;

                Ok(())
            }

            // ignore duplicate connections
            (Some(_conn), tracker::connect::Request::LEN) => Ok(()),
            (_, _) => Err(Error::TrackerResponse),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::time::timeout;
    use vcz_lib::{
        peer::PeerId,
        tracker::{ANNOUNCE_RES_BUF_LEN, action::Action, event::Event},
    };

    use super::*;

    #[tokio::test]
    async fn mock_works() -> Result<(), Error> {
        //
        // connect
        //
        let mut buf = [0u8; tracker::connect::Response::LEN];
        let info_hash = InfoHash::random();
        let req = tracker::connect::Request::default();

        // from instead of new to avoid conflicts with other tests
        let mut mock = MockTracker::from(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
            1338,
        ))
        .await
        .unwrap();
        let mock_addr = mock.socket.local_addr().unwrap();
        let mysocket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = mysocket.local_addr().unwrap();
        println!("my addr {addr:?}");

        mysocket.send_to(&req.serialize()?, mock_addr).await?;

        let _ = timeout(Duration::from_millis(100), mock.run()).await;
        let res_len = mysocket.recv(&mut buf).await?;
        let res = tracker::connect::Response::deserialize(&buf[0..res_len])?;

        assert_eq!(res.connection_id, *mock.peers.get(&addr).unwrap());
        assert_eq!(res.action, req.action);
        assert_eq!(res.transaction_id, req.transaction_id);
        //
        // announce
        //
        // add a fake seeder just to see if it works
        let fake = "127.0.0.1:5678".parse().unwrap();
        let fake_id = PeerId::generate();
        mock.insert_seeder((
            fake_id,
            PeerInfo { connection_id: 123, addr: fake, key: 321 },
        ));

        let mut buf = [0u8; ANNOUNCE_RES_BUF_LEN];
        let peer_id = PeerId::generate();
        let req = tracker::announce::Request {
            connection_id: res.connection_id.to_native(),
            action: Action::Announce,
            transaction_id: rand::random(),
            info_hash: info_hash.clone(),
            peer_id: peer_id.clone(),
            downloaded: 0,
            left: u64::MAX,
            uploaded: 0,
            event: Event::Started,
            ip_address: {
                let ip = addr.ip();
                let ip = ip.as_octets();
                u32::from_be_bytes([ip[0], ip[1], ip[2], ip[3]])
            },
            key: 123,
            num_want: 50,
            port: addr.port(),
            compact: 1,
        };

        mysocket.send_to(&req.serialize()?, mock_addr).await?;
        let _ = timeout(Duration::from_millis(100), mock.run()).await;
        let res_len = mysocket.recv(&mut buf).await?;
        let (res, payload) =
            tracker::announce::Response::deserialize(&buf[0..res_len])?;

        // `peers` will only have `fake` and not include the ip of the
        // caller.
        let peers = tracker::parse_compact_peer_list(false, payload)?;

        println!("< res   {res:?}");
        println!("< peers {peers:?}");

        assert_eq!(peers, [fake]);
        assert_eq!(res.action, Action::Announce);
        assert_eq!(res.transaction_id, req.transaction_id);
        assert_eq!(
            *mock.leechers.get(&peer_id).unwrap(),
            PeerInfo { connection_id: req.connection_id, key: req.key, addr }
        );

        Ok(())
    }
}
