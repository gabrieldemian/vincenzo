use std::{
    fmt::Debug,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs},
    time::Duration,
};

use log::{debug, info, warn};
use tokio::{
    net::UdpSocket,
    sync::mpsc::Sender,
    time::{interval, timeout},
};

use crate::{error::Error, torrent::TorrentMsg};

use super::{announce, connect};

#[derive(Debug)]
pub struct Tracker {
    pub peer_id: [u8; 20],
    pub tracker_addr: SocketAddr,
    /// UDP Socket of the `tracker_addr`
    pub socket: UdpSocket,
    pub connection_id: Option<u64>,
}

impl Tracker {
    const ANNOUNCE_RES_BUF_LEN: usize = 8192;

    /// Bind UDP socket and send a connect handshake,
    /// to one of the trackers.
    pub async fn connect<A: ToSocketAddrs + Debug>(trackers: Vec<A>) -> Result<Self, Error> {
        info!("...trying to connect to 1 of {:?} trackers", trackers.len());

        for tracker in trackers {
            let addrs = tracker
                .to_socket_addrs()
                .map_err(Error::TrackerSocketAddrs)?;

            for tracker_addr in addrs {
                let sock = match Self::new_udp_socket(tracker_addr).await {
                    Ok(sock) => sock,
                    Err(_) => {
                        warn!("could not connect to tracker {tracker_addr}");
                        continue;
                    }
                };
                let mut client = Tracker {
                    peer_id: rand::random(),
                    tracker_addr,
                    socket: sock,
                    connection_id: None,
                };
                if client.connect_exchange().await.is_ok() {
                    info!("connected with tracker addr {tracker_addr}");
                    debug!("DNS of the tracker {:?}", tracker);
                    return Ok(client);
                }
            }
        }
        Err(Error::TrackerNoHosts)
    }

    pub async fn announce_exchange(&self, infohash: [u8; 20]) -> Result<Vec<SocketAddr>, Error> {
        let connection_id = match self.connection_id {
            Some(x) => x,
            None => return Err(Error::TrackerNoConnectionId),
        };

        let req = announce::Request::new(
            connection_id,
            infohash,
            self.peer_id,
            self.socket.local_addr()?.port(),
        );

        debug!("local ip is {}", self.socket.local_addr()?);

        let mut len = 0_usize;
        let mut res = [0u8; Self::ANNOUNCE_RES_BUF_LEN];

        // will try to connect up to 3 times
        // breaking if succesfull
        for i in 0..=2 {
            info!("trying to send announce number {i}...");
            self.socket.send(&req.serialize()).await?;
            match timeout(Duration::new(3, 0), self.socket.recv(&mut res)).await {
                Ok(Ok(lenn)) => {
                    len = lenn;
                    break;
                }
                Err(e) => {
                    warn!("failed to announce {:#?}", e);
                }
                _ => {}
            }
        }

        if len == 0 {
            return Err(Error::TrackerResponse);
        }

        let res = &res[..len];

        // res is the deserialized struct,
        // payload is a byte array of peers,
        // which are in the form of ips and ports
        let (res, payload) = announce::Response::deserialize(res)?;

        if res.transaction_id != req.transaction_id || res.action != req.action {
            return Err(Error::TrackerResponse);
        }

        info!("* announce successful");
        info!("res from announce {:?}", res);

        let peers = Self::parse_compact_peer_list(payload, self.socket.local_addr()?.is_ipv6())?;
        debug!("got peers: {:#?}", peers);

        Ok(peers)
    }

    /// Connect is the first step in getting the file
    async fn connect_exchange(&mut self) -> Result<(), Error> {
        let req = connect::Request::new();
        let mut buf = [0u8; connect::Response::LENGTH];
        let mut len: usize = 0;

        // will try to connect up to 3 times
        // breaking if succesfull
        for i in 0..=2 {
            debug!("sending connect number {i}...");
            self.socket.send(&req.serialize()).await?;

            match timeout(Duration::new(3, 0), self.socket.recv(&mut buf)).await {
                Ok(Ok(lenn)) => {
                    len = lenn;
                    break;
                }
                Err(e) => info!("error receiving {e}"),
                _ => {}
            }
        }

        if len == 0 {
            return Err(Error::TrackerResponse);
        }

        let (res, _) = connect::Response::deserialize(&buf)?;

        info!("received res from tracker {:#?}", res);

        if res.transaction_id != req.transaction_id || res.action != req.action {
            warn!("response not valid!");
            return Err(Error::TrackerResponse);
        }

        self.connection_id.replace(res.connection_id);
        Ok(())
    }

    /// Create an UDP Socket for the given tracker address
    // todo: make this non-blocking
    pub async fn new_udp_socket(addr: SocketAddr) -> Result<UdpSocket, Error> {
        let sock = match addr {
            SocketAddr::V4(_) => UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).await,
            SocketAddr::V6(_) => UdpSocket::bind((Ipv6Addr::UNSPECIFIED, 0)).await,
        }
        .expect("Failed to bind udp socket");
        sock.connect(addr)
            .await
            .expect("Failed to connect to udp socket");

        Ok(sock)
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

    // the addr used to announce will be added, by the tracker,
    // as a peer to the list of peers. This means I need to
    // listen to handshake events with this addr here.
    // and this function needs a Sender to the `Torrent`
    pub async fn run(&self, _tx: Sender<TorrentMsg>) {
        info!("# listening to tracker events...");
        let mut tick_timer = interval(Duration::from_secs(1));

        let mut buf = [0; 1024];
        loop {
            tick_timer.tick().await;
            debug!("tick tracker");
            match self.socket.recv(&mut buf).await {
                Ok(0) => {
                    warn!("peer closed");
                }
                Ok(n) => {
                    info!("datagram {:?}", &buf[..n]);
                }
                _ => {}
            }
        }
    }
}
