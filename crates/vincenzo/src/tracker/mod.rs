//! A tracker is a server that manages peers and stats of multiple torrents.
pub mod action;
pub mod announce;
pub mod connect;
pub mod event;

use super::tracker::action::Action;
use std::{
    fmt::Debug,
    future::Future,
    marker::PhantomData,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    time::Duration,
};

use crate::{error::Error, peer::PeerId, torrent::InfoHash};
use rand::Rng;
use tokio::{
    net::{ToSocketAddrs, UdpSocket},
    select,
    sync::{mpsc, oneshot},
    time::timeout,
};
use tracing::{debug, error, warn};

use self::event::Event;

pub struct Udp;
pub struct Http;

/// The generic `P` stands for "Protocol".
/// Currently, only UDP and HTTP are supported.
#[derive(Debug)]
pub struct Tracker<P> {
    /// Socket of the `tracker_addr`
    /// Peers announcing will send handshakes
    /// to this addr
    pub local_addr: SocketAddr,
    pub peer_addr: SocketAddr,
    pub ctx: TrackerCtx,
    pub rx: mpsc::Receiver<TrackerMsg>,
    marker: PhantomData<P>,
}

pub trait TrackerTrait: Sized {
    fn connect<A>(
        trackers: Vec<A>,
    ) -> impl Future<Output = Result<Self, Error>>
    where
        A: ToSocketAddrs
            + Debug
            + Send
            + Sync
            + 'static
            + std::fmt::Display
            + Clone,
        A::Iter: Send;

    fn connect_exchange(
        &mut self,
        socket: UdpSocket,
    ) -> impl Future<Output = Result<UdpSocket, Error>> + Send;

    fn announce_exchange(
        &mut self,
        info_hash: &InfoHash,
        listen: Option<SocketAddr>,
    ) -> impl Future<Output = Result<(announce::Response, Vec<SocketAddr>), Error>>
           + Send;
}

impl Default for Tracker<Udp> {
    fn default() -> Tracker<Udp> {
        let (tx, rx) = mpsc::channel::<TrackerMsg>(300);

        let peer_id = Self::gen_peer_id();

        Self {
            rx,
            local_addr: "0.0.0.0:0".parse().unwrap(),
            peer_addr: "0.0.0.0:0".parse().unwrap(),
            ctx: TrackerCtx {
                tx: tx.into(),
                peer_id,
                tracker_addr: "".to_owned(),
                connection_id: None,
                local_peer_addr: "0.0.0.0:0".parse().unwrap(),
            },
            marker: PhantomData,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TrackerCtx {
    pub tx: Option<mpsc::Sender<TrackerMsg>>,
    /// Our ID for this connected Tracker
    pub peer_id: PeerId,
    /// UDP Socket of the `socket` in Tracker struct
    pub tracker_addr: String,
    /// Our peer socket addr, peers will send handshakes
    /// to this addr.
    pub local_peer_addr: SocketAddr,
    pub connection_id: Option<u64>,
}

impl Default for TrackerCtx {
    fn default() -> Self {
        TrackerCtx {
            local_peer_addr: "0.0.0.0:0".parse().unwrap(),
            peer_id: Tracker::gen_peer_id(),
            tx: None,
            connection_id: None,
            tracker_addr: "".to_owned(),
        }
    }
}

#[derive(Debug)]
pub enum TrackerMsg {
    Announce {
        event: Event,
        info_hash: InfoHash,
        downloaded: u64,
        uploaded: u64,
        left: u64,
        recipient: Option<oneshot::Sender<Result<announce::Response, Error>>>,
    },
}

impl<P> Tracker<P> {
    const ANNOUNCE_RES_BUF_LEN: usize = 8192;
}

impl Tracker<Udp> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Bind UDP socket and send a connect handshake,
    /// to one of the trackers.
    // todo: get a new tracker if download is stale
    #[tracing::instrument(skip(trackers), name = "tracker::connect")]
    pub async fn connect<A>(trackers: Vec<A>) -> Result<Self, Error>
    where
        A: ToSocketAddrs
            + Debug
            + Send
            + Sync
            + 'static
            + std::fmt::Display
            + Clone,
        A::Iter: Send,
    {
        debug!("...trying to connect to {:?} trackers", trackers.len());

        // Connect to all trackers, return on the first
        // successful handshake.
        for tracker_addr in trackers {
            debug!("trying to connect {tracker_addr:?}");

            let socket = match Self::new_udp_socket(tracker_addr.clone()).await
            {
                Ok(socket) => socket,
                Err(_) => {
                    debug!("could not connect to tracker");
                    continue;
                }
            };
            let (tracker_tx, tracker_rx) = mpsc::channel::<TrackerMsg>(300);
            let mut tracker = Tracker {
                ctx: TrackerCtx {
                    tracker_addr: tracker_addr.to_string(),
                    tx: tracker_tx.into(),
                    peer_id: Tracker::gen_peer_id(),
                    local_peer_addr: "0.0.0.0:0".parse().unwrap(),
                    connection_id: None,
                },
                rx: tracker_rx,
                local_addr: socket.local_addr().unwrap(),
                peer_addr: socket.peer_addr().unwrap(),
                marker: PhantomData,
            };
            if tracker.connect_exchange(socket).await.is_ok() {
                debug!("announced to tracker {tracker_addr}");
                return Ok(tracker);
            }
        }

        error!(
            "Could not connect to any tracker, all trackers rejected the \
             connection."
        );
        Err(Error::TrackerNoHosts)
    }

    #[tracing::instrument(skip(self))]
    async fn connect_exchange(
        &mut self,
        socket: UdpSocket,
    ) -> Result<UdpSocket, Error> {
        let req = connect::Request::new();
        let mut buf = [0u8; connect::Response::LENGTH];
        let mut len: usize = 0;

        // will try to connect up to 3 times
        // breaking if succesfull
        for i in 0..=2 {
            debug!("sending connect number {i}...");
            socket.send(&req.serialize()).await?;

            match timeout(Duration::new(5, 0), socket.recv(&mut buf)).await {
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

        debug!("received res from tracker {res:#?}");

        if res.transaction_id != req.transaction_id || res.action != req.action
        {
            error!("response is not valid {res:?}");
            return Err(Error::TrackerResponse);
        }

        self.ctx.connection_id.replace(res.connection_id);
        Ok(socket)
    }

    /// Attempts to send an "announce_request" to the tracker
    #[tracing::instrument(skip(self, info_hash))]
    pub async fn announce_exchange(
        &mut self,
        info_hash: &InfoHash,
        listen: Option<SocketAddr>,
    ) -> Result<(announce::Response, Vec<SocketAddr>), Error> {
        let socket = UdpSocket::bind(self.local_addr).await?;
        socket.connect(self.peer_addr).await?;

        let connection_id = match self.ctx.connection_id {
            Some(x) => x,
            None => return Err(Error::TrackerNoConnectionId),
        };

        let local_peer_socket = {
            match listen {
                Some(listen) => SocketAddr::new(
                    IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)),
                    listen.port(),
                ),
                None => {
                    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)), 0)
                }
            }
        };

        let req = announce::Request::new(
            connection_id,
            info_hash.clone(),
            self.ctx.peer_id.clone(),
            // local_peer_socket.ip() as u32,
            0,
            local_peer_socket.port(),
            Event::Started,
        );

        debug!("announce req {req:?}");

        self.ctx.local_peer_addr = local_peer_socket;

        debug!("local_peer_addr {:?}", self.ctx.local_peer_addr);

        debug!("local ip is {}", socket.local_addr()?);

        let mut len = 0_usize;
        let mut res = [0u8; Self::ANNOUNCE_RES_BUF_LEN];

        // will try to connect up to 3 times
        // breaking if succesfull
        for i in 0..=2 {
            debug!("trying to send announce number {i}...");
            socket.send(&req.serialize()).await?;
            match timeout(Duration::new(3, 0), socket.recv(&mut res)).await {
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

        if res.transaction_id != req.transaction_id || res.action != req.action
        {
            return Err(Error::TrackerResponse);
        }

        debug!("* announce successful");
        debug!("res from announce {:#?}", res);

        let peers = Self::parse_compact_peer_list(
            payload,
            socket.peer_addr()?.is_ipv6(),
        )?;

        Ok((res, peers))
    }

    /// Connect is the first step in getting the file
    /// Create an UDP Socket for the given tracker address
    #[tracing::instrument(skip(addr))]
    pub async fn new_udp_socket<A: ToSocketAddrs + std::fmt::Debug>(
        addr: A,
    ) -> Result<UdpSocket, Error> {
        let socket = UdpSocket::bind("0.0.0.0:0").await;
        if let Ok(socket) = socket {
            if socket.connect(addr).await.is_ok() {
                return Ok(socket);
            }
            return Err(Error::TrackerSocketConnect);
        }
        Err(Error::TrackerSocketAddr)
    }

    #[tracing::instrument(skip(buf, is_ipv6))]
    fn parse_compact_peer_list(
        buf: &[u8],
        is_ipv6: bool,
    ) -> Result<Vec<SocketAddr>, Error> {
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
                IpAddr::from(std::net::Ipv4Addr::new(
                    ip[0], ip[1], ip[2], ip[3],
                ))
            };

            let port = u16::from_be_bytes(
                port.try_into().expect("iterator guarantees bounds are OK"),
            );

            peer_list.push((ip, port).into());
        }

        debug!("ips of peers addrs {peer_list:#?}");
        let peers: Vec<SocketAddr> = peer_list.into_iter().collect();

        Ok(peers)
    }
    #[tracing::instrument(skip(self))]
    pub async fn announce_msg(
        &self,
        event: Event,
        info_hash: InfoHash,
        downloaded: u64,
        uploaded: u64,
        left: u64,
    ) -> Result<announce::Response, Error> {
        debug!("announcing {event:#?} to tracker");
        let socket = UdpSocket::bind(self.local_addr).await?;
        socket.connect(self.peer_addr).await?;

        let req = announce::Request {
            connection_id: self.ctx.connection_id.unwrap_or(0),
            action: Action::Announce.into(),
            transaction_id: rand::rng().random(),
            info_hash,
            peer_id: self.ctx.peer_id.clone(),
            downloaded,
            left,
            uploaded,
            event: event.into(),
            ip_address: 0,
            num_want: u32::MAX,
            port: self.local_addr.port(),
            compact: 1,
        };

        let mut len = 0_usize;
        let mut res = [0u8; Self::ANNOUNCE_RES_BUF_LEN];

        // will try to connect up to 3 times
        // breaking if succesfull
        for _ in 0..=2 {
            socket.send(&req.serialize()).await?;
            match timeout(Duration::new(3, 0), socket.recv(&mut res)).await {
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

        let res = &res[..len];

        let (res, _) = announce::Response::deserialize(res)?;

        Ok(res)
    }

    #[tracing::instrument(skip(self))]
    pub async fn run(&mut self) -> Result<(), Error> {
        debug!("running tracker");
        loop {
            select! {
                Some(msg) = self.rx.recv() => {
                    match msg {
                        TrackerMsg::Announce {
                            info_hash,
                            downloaded,
                            uploaded,
                            recipient,
                            event,
                            left,
                        } => {
                            let r = self
                                .announce_msg(event.clone(), info_hash, downloaded, uploaded, left)
                                .await;

                            if let Some(recipient) = recipient {
                                let _ = recipient.send(r);
                            }

                            if event == Event::Stopped {
                                return Ok(());
                            }
                        }
                    }
                }
            }
        }
    }
    /// Peer ids should be prefixed with "vcz".
    pub fn gen_peer_id() -> PeerId {
        let mut peer_id = [0; 20];
        peer_id[..3].copy_from_slice(b"vcz");
        peer_id[3..].copy_from_slice(&rand::random::<[u8; 17]>());
        peer_id.into()
    }
}

#[cfg(test)]
mod tests {
    use super::Tracker;

    #[test]
    fn peer_ids_prefixed_with_vcz() {
        // Poor man's fuzzing.
        let peer_id = Tracker::gen_peer_id();
        let first_three = &peer_id.to_string()[..3];

        assert_eq!(first_three, "vcz");
        assert_eq!(peer_id.to_string().len(), 20);
    }
}
