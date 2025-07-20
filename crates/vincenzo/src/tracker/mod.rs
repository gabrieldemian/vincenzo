//! A tracker is a server that manages peers and stats of multiple torrents.
pub mod action;
pub mod announce;
pub mod connect;
pub mod event;

use super::tracker::action::Action;
use std::{
    fmt::Debug,
    future::Future,
    net::{IpAddr, SocketAddr},
    time::Duration,
};

use crate::{
    config::CONFIG, error::Error, metainfo::Info, peer::PeerId,
    torrent::InfoHash,
};
use rand::Rng;
use tokio::{
    net::{ToSocketAddrs, UdpSocket},
    select,
    sync::{mpsc, oneshot},
    time::timeout,
};
use tracing::{debug, error, warn};

use self::event::Event;

pub trait Protocol {}

pub struct Udp {
    pub socket: UdpSocket,
}
pub struct Http;

impl Protocol for Udp {}
impl Protocol for Http {}

static ANNOUNCE_RES_BUF_LEN: usize = 8192;

/// Trait that all [`Tracker`]s must implement.
pub trait TrackerTrait: Sized {
    /// Before doing an `announce`, a client must perform a connect
    /// exchange, to obtain a connection_id.
    fn connect(
        &mut self,
    ) -> impl Future<Output = Result<connect::Response, Error>> + Send;

    /// An announce is the communication of the local peer to the tracker, for
    /// example:
    /// - When the torrent starts, we announce with [`Event::Started`] to get
    ///   the list of peers for this torrent.
    /// - To update the tracker with the download stats.
    /// - Etc.
    ///
    /// The buffer needs to be decoded depending on the event type, for an
    /// [`Event::Started`] the `parse_compact_peer_list` should be used.
    fn announce(
        &self,
        event: Event,
    ) -> impl Future<Output = Result<(announce::Response, Vec<u8>), Error>>;

    /// Support for BEP23
    fn parse_compact_peer_list(
        &self,
        buf: &[u8],
    ) -> Result<Vec<SocketAddr>, Error>;

    /// Try to connect to one tracker, in order, and return Self.
    fn connect_to_tracker<A>(
        trackers: Vec<A>,
        info_hash: InfoHash,
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
}

/// The generic `P` stands for "Protocol".
/// Currently, only UDP and HTTP are supported.
#[derive(Debug)]
pub struct Tracker<P: Protocol> {
    pub ctx: TrackerCtx,
    pub info: Option<Info>,
    pub info_hash: InfoHash,
    pub rx: mpsc::Receiver<TrackerMsg>,
    pub connection_id: u64,

    /// Remote addr of the tracker.
    pub tracker_addr: SocketAddr,

    state: P,
}

#[derive(Debug, Clone)]
pub struct TrackerCtx {
    pub tx: mpsc::Sender<TrackerMsg>,

    /// Our ID for this connected Tracker
    pub peer_id: PeerId,

    pub downloaded: u64,
    pub uploaded: u64,
    pub left: u64,
}

#[derive(Debug)]
pub enum TrackerMsg {
    Announce {
        event: Event,
        recipient: Option<oneshot::Sender<(announce::Response, Vec<u8>)>>,
    },
    Increment {
        downloaded: u64,
        uploaded: u64,
    },
    Info(Info),
}

impl TrackerTrait for Tracker<Udp> {
    #[tracing::instrument(skip(self, buf))]
    fn parse_compact_peer_list(
        &self,
        buf: &[u8],
    ) -> Result<Vec<SocketAddr>, Error> {
        let mut peer_list = Vec::<SocketAddr>::new();

        let is_ipv6 = self.tracker_addr.is_ipv6();

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

    /// Bind UDP socket and send a connect handshake,
    /// to one of the trackers.
    // todo: get a new tracker if download is stale
    #[tracing::instrument(skip(trackers))]
    async fn connect_to_tracker<A>(
        trackers: Vec<A>,
        info_hash: InfoHash,
    ) -> Result<Self, Error>
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
        debug!("trying to connect to {:?} trackers", trackers.len());

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

            let (tracker_tx, tracker_rx) = mpsc::channel::<TrackerMsg>(100);

            let mut tracker = Tracker {
                ctx: TrackerCtx {
                    tx: tracker_tx,
                    peer_id: Tracker::gen_peer_id(),
                    downloaded: 0,
                    uploaded: 0,
                    left: 0,
                },
                tracker_addr: socket.peer_addr().unwrap(),
                info_hash: info_hash.clone(),
                info: None,
                connection_id: 0,
                state: Udp { socket },
                rx: tracker_rx,
            };

            if Tracker::connect(&mut tracker).await.is_ok() {
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
    async fn connect(&mut self) -> Result<connect::Response, Error> {
        let req = connect::Request::new();
        let mut buf = [0u8; connect::Response::LENGTH];
        let mut len: usize = 0;

        self.state.socket.send(&req.serialize()).await?;

        let mut retransmit = 30;

        for i in 1..=7 {
            match timeout(
                Duration::from_secs(retransmit),
                self.state.socket.recv(&mut buf),
            )
            .await
            {
                Ok(Ok(lenn)) => {
                    len = lenn;
                    break;
                }
                Ok(Err(e)) => {
                    error!("error connecting to tracker: {e:?}");
                    return Err(Error::TrackerResponse);
                }
                Err(_) => {
                    retransmit = 15 * 2_u64.pow(i);
                    debug!(
                        "tracker connect request was lost, trying again in \
                         {retransmit}s"
                    );
                    self.state.socket.send(&req.serialize()).await?;
                }
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

        self.connection_id = res.connection_id;

        Ok(res)
    }

    #[tracing::instrument(skip(self))]
    async fn announce(
        &self,
        event: Event,
    ) -> Result<(announce::Response, Vec<u8>), Error> {
        debug!("announcing {event:#?} to tracker");

        let req = announce::Request {
            connection_id: self.connection_id,
            action: Action::Announce.into(),
            transaction_id: rand::rng().random(),
            info_hash: self.info_hash.clone(),
            peer_id: self.ctx.peer_id.clone(),
            downloaded: self.ctx.downloaded,
            left: self.ctx.left,
            uploaded: self.ctx.uploaded,
            event: event.into(),
            ip_address: 0,
            num_want: u32::MAX,
            port: CONFIG.local_peer_port,
            compact: 1,
        };

        let mut len = 0_usize;
        let mut res = [0u8; ANNOUNCE_RES_BUF_LEN];

        let mut retransmit = 30;

        self.state.socket.send(&req.serialize()).await?;

        for i in 1..=7 {
            match timeout(
                Duration::from_secs(retransmit),
                self.state.socket.recv(&mut res),
            )
            .await
            {
                Ok(Ok(lenn)) => {
                    len = lenn;
                    break;
                }
                Ok(Err(e)) => {
                    error!("error announcing to tracker: {e:?}");
                    return Err(Error::TrackerResponse);
                }
                Err(_) => {
                    retransmit = 15 * 2_u64.pow(i);
                    debug!(
                        "tracker announce request was lost, trying again in \
                         {retransmit}s"
                    );
                    self.state.socket.send(&req.serialize()).await?;
                }
            }
        }

        let res = &res[..len];
        let (res, payload) = announce::Response::deserialize(res)?;

        if res.transaction_id != req.transaction_id || res.action != req.action
        {
            return Err(Error::TrackerResponse);
        }

        Ok((res, payload.to_owned()))
    }
}

impl Tracker<Udp> {
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

    #[tracing::instrument(skip(self))]
    pub async fn run(&mut self) -> Result<(), Error> {
        debug!("running tracker");
        loop {
            select! {
                Some(msg) = self.rx.recv() => {
                    match msg {
                        TrackerMsg::Info(info) => self.info = Some(info),
                        TrackerMsg::Increment {downloaded, uploaded} => {
                            self.ctx.downloaded += downloaded;
                            self.ctx.uploaded += uploaded;

                            if let Some(info) = &self.info {
                                let left =
                                    if self.ctx.downloaded < info.get_size()
                                        { info.get_size() - self.ctx.downloaded }
                                    else { 0 };

                                self.ctx.left = left;
                            }
                        }
                        TrackerMsg::Announce {
                            recipient,
                            event,
                        } => {
                            let res = self
                                .announce(event.clone())
                                .await?;

                            if let Some(recipient) = recipient {
                                let _ = recipient.send(res);
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
    fn gen_peer_id() -> PeerId {
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
