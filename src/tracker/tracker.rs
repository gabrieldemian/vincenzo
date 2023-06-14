use std::{
    fmt::Debug,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs},
    time::Duration,
};

use futures::future::join_all;
use log::{debug, info, warn};
use tokio::{
    net::UdpSocket,
    select, spawn,
    sync::mpsc::Sender,
    task::JoinHandle,
    time::{interval, timeout},
};

use crate::{error::Error, peer::Peer, torrent::TorrentMsg};

use super::{announce, connect};

#[derive(Debug, Clone)]
pub struct Tracker {
    /// UDP Socket of the `tracker_addr`
    /// Peers announcing will send handshakes
    /// to this addr
    pub socket: SocketAddr,
    pub ctx: TrackerCtx,
}

#[derive(Debug, Clone)]
pub struct TrackerCtx {
    /// Our ID for this connected Tracker
    pub peer_id: [u8; 20],
    /// UDP Socket of the `socket` in Tracker
    /// Peers announcing will send handshakes
    /// to this addr
    pub tracker_addr: SocketAddr,
    pub connection_id: Option<u64>,
}

impl Default for TrackerCtx {
    fn default() -> Self {
        Self {
            peer_id: [0u8; 20],
            tracker_addr: "0.0.0.0:0".parse().unwrap(),
            connection_id: None,
        }
    }
}

impl Tracker {
    const ANNOUNCE_RES_BUF_LEN: usize = 8192;

    /// Bind UDP socket and send a connect handshake,
    /// to one of the trackers.
    pub async fn connect<A: ToSocketAddrs + Debug + Send + Sync + 'static>(
        trackers: Vec<A>,
    ) -> Result<Self, Error>
    where
        <A as std::net::ToSocketAddrs>::Iter: std::marker::Send,
    {
        info!("...trying to connect to 1 of {:?} trackers", trackers.len());
        let mut handles: Vec<JoinHandle<Result<Tracker, Error>>> = vec![];

        for tracker in trackers {
            let handle = spawn(async move {
                debug!("trying to tracker {tracker:?}");

                let addrs = tracker
                    .to_socket_addrs()
                    .map_err(Error::TrackerSocketAddrs)?;

                for tracker_addr in addrs {
                    debug!("trying to connect {tracker_addr:?}");
                    let socket = match Self::new_udp_socket(tracker_addr).await {
                        Ok(socket) => socket,
                        Err(_) => {
                            warn!("could not connect to tracker {tracker_addr}");
                            continue;
                        }
                    };
                    let mut tracker = Tracker {
                        ctx: TrackerCtx {
                            peer_id: rand::random(),
                            tracker_addr,
                            connection_id: None,
                        },
                        socket: socket.local_addr().unwrap(),
                    };
                    if tracker.connect_exchange().await.is_ok() {
                        info!("connected with tracker addr {tracker_addr}");
                        debug!("DNS of the tracker {:?}", tracker);
                        return Ok(tracker);
                    }
                }
                Err(Error::TrackerNoHosts)
            });
            handles.push(handle);
        }
        let r = join_all(handles).await;
        let r: Vec<Tracker> = r
            .into_iter()
            .filter_map(|x| {
                if let Ok(Ok(x)) = x {
                    return Some(x);
                }
                None
            })
            .collect();

        if r.is_empty() {
            return Err(Error::TrackerNoHosts);
        }

        Ok(r[0].clone())
    }

    pub async fn announce_exchange(&self, infohash: [u8; 20]) -> Result<Vec<Peer>, Error> {
        let connection_id = match self.ctx.connection_id {
            Some(x) => x,
            None => return Err(Error::TrackerNoConnectionId),
        };

        let req = announce::Request::new(
            connection_id,
            infohash,
            self.ctx.peer_id,
            self.socket.port(),
        );

        debug!("local ip is {}", self.socket);

        let mut len = 0_usize;
        let mut res = [0u8; Self::ANNOUNCE_RES_BUF_LEN];

        let socket = Self::new_udp_socket(self.socket).await.unwrap();

        // will try to connect up to 3 times
        // breaking if succesfull
        for i in 0..=2 {
            info!("trying to send announce number {i}...");
            socket.send(&req.serialize()).await?;
            match timeout(Duration::new(3, 0), socket.recv(&mut res)).await {
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

        let peers = Self::parse_compact_peer_list(payload, self.socket.is_ipv6())?;
        debug!("got peers: {:#?}", peers);

        Ok(peers)
    }

    /// Connect is the first step in getting the file
    async fn connect_exchange(&mut self) -> Result<(), Error> {
        let req = connect::Request::new();
        let mut buf = [0u8; connect::Response::LENGTH];
        let mut len: usize = 0;

        let socket = Self::new_udp_socket(self.socket).await.unwrap();

        // will try to connect up to 3 times
        // breaking if succesfull
        for i in 0..=2 {
            debug!("sending connect number {i}...");
            socket.send(&req.serialize()).await?;

            match timeout(Duration::new(3, 0), socket.recv(&mut buf)).await {
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

        self.ctx.connection_id.replace(res.connection_id);
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

    fn parse_compact_peer_list(buf: &[u8], is_ipv6: bool) -> Result<Vec<Peer>, Error> {
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

        let peers: Vec<Peer> = peer_list.into_iter().map(|p| p.into()).collect();

        Ok(peers)
    }

    // the addr used to announce will be added, by the tracker,
    // as a peer to the list of peers. This means I need to
    // listen to handshake events with this addr here.
    // and this function needs a Sender to the `Torrent`
    #[tracing::instrument]
    pub async fn run(&self, _tx: Sender<TorrentMsg>) {
        info!("# listening to tracker events...");
        let mut tick_timer = interval(Duration::from_secs(1));

        let socket = Self::new_udp_socket(self.socket).await.unwrap();

        let mut buf = [0; 1024];
        loop {
            select! {
                _ = tick_timer.tick() => {
                    debug!("tick tracker");
                }
                Ok(n) = socket.recv(&mut buf) => {
                    match n {
                        0 => {
                            warn!("peer closed");
                        }
                        n => {
                            info!("datagram {:?}", &buf[..n]);
                        }
                    }
                }
            }
        }
    }
}
