use std::{
    fmt::Debug,
    net::{IpAddr, SocketAddr},
    time::Duration,
};

use tokio::{
    io::AsyncReadExt,
    net::{TcpListener, ToSocketAddrs, UdpSocket},
    select, spawn,
    sync::mpsc,
    time::{interval, timeout},
};
use tracing::{debug, info, warn};

use crate::{error::Error, peer::Peer, tcp_wire::messages::Handshake};

use super::{announce, connect};

#[derive(Debug)]
pub struct Tracker {
    /// UDP Socket of the `tracker_addr`
    /// Peers announcing will send handshakes
    /// to this addr
    pub socket: UdpSocket,
    pub local_addr: SocketAddr,
    pub peer_addr: SocketAddr,
    pub ctx: TrackerCtx,
}

#[derive(Debug, Clone)]
pub struct TrackerCtx {
    /// Our ID for this connected Tracker
    pub peer_id: [u8; 20],
    /// UDP Socket of the `socket` in Tracker struct
    pub tracker_addr: String,
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
    // todo: return only tracker woth the most seeders?
    #[tracing::instrument(skip(trackers))]
    pub async fn connect<A>(trackers: Vec<A>) -> Result<Self, Error>
    where
        A: ToSocketAddrs + Debug + Send + Sync + 'static + std::fmt::Display + Clone,
        A::Iter: Send,
    {
        info!("...trying to connect to {:?} trackers", trackers.len());

        let (tx, mut rx) = mpsc::channel::<Tracker>(30);

        // Connect to all trackers, return on the first
        // succesful handshake
        for tracker_addr in trackers {
            debug!("trying to connect {tracker_addr:?}");
            let tx = tx.clone();

            spawn(async move {
                let socket = match Self::new_udp_socket(tracker_addr.clone()).await {
                    Ok(socket) => socket,
                    Err(_) => {
                        warn!("could not connect to tracker");
                        return Ok::<(), Error>(());
                    }
                };
                let mut tracker = Tracker {
                    ctx: TrackerCtx {
                        peer_id: rand::random(),
                        tracker_addr: tracker_addr.to_string(),
                        connection_id: None,
                    },
                    local_addr: socket.local_addr().unwrap(),
                    peer_addr: socket.peer_addr().unwrap(),
                    socket,
                };
                if tracker.connect_exchange().await.is_ok() {
                    info!("announced to tracker {tracker_addr}");
                    debug!("DNS of the tracker {tracker:#?}");
                    if tx.send(tracker).await.is_err() {
                        return Ok(());
                    };
                }
                Ok(())
            });
        }

        while let Some(tracker) = rx.recv().await {
            debug!("Connected and announced to tracker {tracker:#?}");
            return Ok(tracker);
        }

        Err(Error::TrackerNoHosts)
    }

    #[tracing::instrument(skip(self))]
    async fn connect_exchange(&mut self) -> Result<(), Error> {
        let req = connect::Request::new();
        let mut buf = [0u8; connect::Response::LENGTH];
        let mut len: usize = 0;

        // will try to connect up to 3 times
        // breaking if succesfull
        for i in 0..=2 {
            debug!("sending connect number {i}...");
            self.socket.send(&req.serialize()).await?;

            match timeout(Duration::new(5, 0), self.socket.recv(&mut buf)).await {
                Ok(Ok(lenn)) => {
                    len = lenn;
                    break;
                }
                Err(e) => {
                    debug!("error receiving connect response, {e}");
                }
                _ => {}
            }
        }

        if len == 0 {
            return Err(Error::TrackerResponse);
        }

        let (res, _) = connect::Response::deserialize(&buf)?;

        info!("received res from tracker {res:#?}");

        if res.transaction_id != req.transaction_id || res.action != req.action {
            warn!("response not valid!");
            return Err(Error::TrackerResponse);
        }

        self.ctx.connection_id.replace(res.connection_id);
        Ok(())
    }

    /// Attempts to send an "announce_request" to the tracker
    #[tracing::instrument(skip(self))]
    pub async fn announce_exchange(&self, infohash: [u8; 20]) -> Result<Vec<Peer>, Error> {
        let connection_id = match self.ctx.connection_id {
            Some(x) => x,
            None => return Err(Error::TrackerNoConnectionId),
        };

        let req = announce::Request::new(
            connection_id,
            infohash,
            self.ctx.peer_id,
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
                    warn!("failed to announce {e:#?}");
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

        info!("* announce successful with {self:?}");
        info!("res from announce {:#?}", res);

        let peers = Self::parse_compact_peer_list(payload, self.socket.peer_addr()?.is_ipv6())?;

        Ok(peers)
    }

    /// Connect is the first step in getting the file
    /// Create an UDP Socket for the given tracker address
    #[tracing::instrument(skip(addr))]
    pub async fn new_udp_socket<A: ToSocketAddrs + std::fmt::Debug>(
        addr: A,
    ) -> Result<UdpSocket, Error> {
        let socket = UdpSocket::bind("0.0.0.0:0").await;
        if let Ok(socket) = socket {
            if let Ok(_) = socket.connect(addr).await {
                return Ok(socket);
            }
            return Err(Error::TrackerSocketConnect);
        }
        Err(Error::TrackerSocketAddr)
    }

    #[tracing::instrument(skip(buf))]
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

        info!("ips of peers {peer_list:#?}");
        let peers: Vec<Peer> = peer_list.into_iter().map(|p| p.into()).collect();

        Ok(peers)
    }

    // the addr used to announce will be added, by the tracker,
    // as a peer to the list of peers. This means I need to
    // listen to handshake events with this addr here.
    // and this function needs a Sender to the `Torrent`
    #[tracing::instrument]
    pub async fn run(local_addr: SocketAddr, peer_addr: SocketAddr) -> Result<(), Error> {
        info!("# listening to tracker events...");
        // let mut tick_timer = interval(Duration::from_secs(1));

        info!("my ip is {local_addr:?}");

        // let mut buf = [0; 19834];
        // let tracker_socket = UdpSocket::bind(local_addr).await?;
        // socket.connect(peer_addr).await?;

        let local_peer_socket = TcpListener::bind(local_addr).await?;
        info!("my local socket is {local_peer_socket:?}");

        loop {
            let (mut socket, addr) = local_peer_socket.accept().await?;
            info!("received connection from {addr}");

            spawn(async move {
                let mut buf = vec![0; 19834];

                loop {
                    match socket.read(&mut buf).await {
                        Ok(0) => {
                            warn!("The remote {addr} has closed connection");
                        }
                        Ok(n) => {
                            let buf = &buf[..n];
                            info!("The remote {addr} sent:");
                            info!("{buf:?}");
                            let handshake = Handshake::deserialize(&buf);

                            match handshake {
                                Ok(handshake) => {
                                    info!("received a handshake {handshake:#?}");
                                }
                                Err(_) => info!("the bytes are not a valid handshake"),
                            };
                        }
                        Err(e) => {
                            warn!("Error on remote {addr}");
                            warn!("{e:#?}");
                            return;
                        }
                    }
                }
            });
        }

        // loop {
        //     select! {
        //         _ = tick_timer.tick() => {
        //             // debug!("tick tracker");
        //         }
        //         Ok(n) = socket.recv(&mut buf) => {
        //             match n {
        //                 0 => {
        //                     warn!("peer closed");
        //                 }
        //                 n => {
        //                     info!("datagram {:?}", &buf[..n]);
        //                 }
        //             }
        //         }
        //     }
        // }
    }
}
