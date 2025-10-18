//! A tracker is a server that manages peers and stats of multiple torrents.
pub mod action;
pub mod announce;
pub mod connect;
pub mod event;

use self::event::Event;
use crate::{
    config::ResolvedConfig,
    error::Error,
    peer::PeerId,
    torrent::{InfoHash, Stats, TorrentMsg},
};
use std::{
    fmt::Debug,
    future::Future,
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::Duration,
};
use tokio::{
    net::{ToSocketAddrs, UdpSocket},
    select,
    sync::{broadcast, mpsc, oneshot},
    time::{Instant, interval_at, timeout},
};
use tracing::{debug, error};

pub trait Protocol {}

pub struct Udp {
    pub socket: UdpSocket,
}
pub struct Http;

impl Protocol for Udp {}
impl Protocol for Http {}

pub static ANNOUNCE_RES_BUF_LEN: usize = 2_usize.pow(11);

/// Trait that all [`Tracker`]s must implement.
pub trait TrackerTrait: Sized {
    /// Before doing an `announce`, a client must perform a connect
    /// exchange, to obtain a connection_id.
    fn connect(&mut self) -> impl Future<Output = Result<(), Error>> + Send;

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
        &mut self,
        event: Event,
        downloaded: u64,
        uploaded: u64,
        left: u64,
    ) -> impl Future<Output = Result<(Stats, Vec<SocketAddr>), Error>>;

    /// Support for BEP23
    fn parse_compact_peer_list(
        &self,
        buf: &[u8],
    ) -> Result<Vec<SocketAddr>, Error>;

    /// Try to connect to one tracker, in order, and return Self.
    fn connect_to_tracker(
        tracker: &str,
        info_hash: InfoHash,
        local_peer_id: PeerId,
        rx: broadcast::Receiver<TrackerMsg>,
        torrent_tx: mpsc::Sender<TorrentMsg>,
        config: Arc<ResolvedConfig>,
    ) -> impl Future<Output = Result<Self, Error>>;
}

/// The generic `P` stands for "Protocol".
/// Currently, only UDP and HTTP are supported.
pub struct Tracker<P: Protocol> {
    pub tracker_addr: SocketAddr,
    pub local_peer_id: PeerId,
    pub info_hash: InfoHash,
    pub rx: broadcast::Receiver<TrackerMsg>,
    pub connection_id: u64,
    pub interval: u32,
    pub torrent_tx: mpsc::Sender<TorrentMsg>,
    config: Arc<ResolvedConfig>,
    state: P,
}

#[derive(Debug, Clone)]
pub enum TrackerMsg {
    Announce { event: Event, downloaded: u64, uploaded: u64, left: u64 },
}

impl TrackerTrait for Tracker<Udp> {
    async fn connect(&mut self) -> Result<(), Error> {
        let req = connect::Request::new();
        let mut buf = [0u8; connect::Response::LEN];
        let mut len: usize = 0;

        self.state.socket.send(&req.serialize()?).await?;
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
                    self.state.socket.send(&req.serialize()?).await?;
                }
            }
        }

        if len == 0 {
            return Err(Error::ResponseLen);
        }

        let res = connect::Response::deserialize(&buf)?;

        tracing::trace!("received res from tracker {res:#?}");

        if res.transaction_id != req.transaction_id || res.action != req.action
        {
            error!("response is not valid {res:?}");
            return Err(Error::TrackerResponse);
        }

        self.connection_id = res.connection_id.into();

        Ok(())
    }

    async fn announce(
        &mut self,
        event: Event,
        downloaded: u64,
        uploaded: u64,
        left: u64,
    ) -> Result<(Stats, Vec<SocketAddr>), Error> {
        tracing::trace!("announcing {event:#?} to tracker");

        let req = announce::Request {
            connection_id: self.connection_id,
            info_hash: self.info_hash.clone(),
            peer_id: self.local_peer_id.clone(),
            downloaded,
            left,
            uploaded,
            event,
            action: action::Action::Announce,
            transaction_id: rand::random(),
            ip_address: 0,
            key: self.config.key,
            num_want: self.config.max_torrent_peers,
            port: self.config.local_peer_port,
            compact: 1,
        };
        debug!("{req:?}");

        let mut len = 0_usize;
        let mut res = [0u8; ANNOUNCE_RES_BUF_LEN];
        let mut retransmit = 30;

        self.state.socket.send(&req.serialize()?).await?;

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
                    self.state.socket.send(&req.serialize()?).await?;
                }
            }
        }

        let res = &res[..len];
        let (res, payload) = announce::Response::deserialize(res)?;

        if res.transaction_id != req.transaction_id || res.action != req.action
        {
            return Err(Error::TrackerResponse);
        }

        self.interval = res.interval.into();

        let peers = self.parse_compact_peer_list(payload)?;

        let stats = Stats {
            interval: res.interval.into(),
            seeders: res.seeders.into(),
            leechers: res.leechers.into(),
        };

        Ok((stats, peers))
    }

    fn parse_compact_peer_list(
        &self,
        buf: &[u8],
    ) -> Result<Vec<SocketAddr>, Error> {
        let is_ipv6 = self.tracker_addr.is_ipv6();
        self::parse_compact_peer_list(is_ipv6, buf)
    }

    /// Bind UDP socket and send a connect handshake,
    /// to one of the trackers.
    async fn connect_to_tracker(
        tracker: &str,
        info_hash: InfoHash,
        local_peer_id: PeerId,
        rx: broadcast::Receiver<TrackerMsg>,
        torrent_tx: mpsc::Sender<TorrentMsg>,
        config: Arc<ResolvedConfig>,
    ) -> Result<Self, Error> {
        let socket = match Self::new_udp_socket(tracker).await {
            Ok(socket) => socket,
            Err(_) => {
                debug!("could not connect to tracker");
                return Err(Error::TrackerResponse);
            }
        };

        let mut tracker = Tracker {
            tracker_addr: socket.peer_addr().unwrap(),
            interval: 0,
            local_peer_id,
            info_hash: info_hash.clone(),
            connection_id: 0,
            state: Udp { socket },
            torrent_tx,
            rx,
            config,
        };

        if Tracker::connect(&mut tracker).await.is_ok() {
            return Ok(tracker);
        }

        Err(Error::TrackerNoHosts)
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

    #[tracing::instrument(name = "tracker", skip_all,
        fields(addr = %self.tracker_addr)
    )]
    pub async fn run(&mut self) -> Result<(), Error> {
        tracing::trace!("running tracker");

        let mut interval = interval_at(
            Instant::now() + Duration::from_secs(self.interval.into()),
            Duration::from_secs(self.interval.into()),
        );

        loop {
            select! {
                _ = interval.tick() => {
                    let (otx, orx) = oneshot::channel();
                    self.torrent_tx.send(TorrentMsg::GetAnnounceData(otx)).await?;
                    let (downloaded, uploaded, left) = orx.await?;

                    let (stats, peers) = self
                        .announce(Event::None, downloaded, uploaded, left)
                        .await?;

                    interval.reset_at(
                        Instant::now() + Duration::from_secs(stats.interval as u64)
                    );

                    self.torrent_tx.send(TorrentMsg::AddIdlePeers(
                        peers.into_iter().collect()
                    ))
                    .await?;
                }
                Ok(msg) = self.rx.recv() => {
                    match msg {
                        TrackerMsg::Announce {
                            event,
                            downloaded,
                            uploaded,
                            left,
                        } => {
                            let (_stats, peers) = self
                                .announce(event, downloaded, uploaded, left)
                                .await?;

                            if event == Event::Stopped {
                                return Ok(());
                            }

                            self.torrent_tx.send(TorrentMsg::AddIdlePeers(
                                peers.into_iter().collect()
                            ))
                            .await?;
                        }
                    }
                }
            }
        }
    }
}

pub fn parse_compact_peer_list(
    is_ipv6: bool,
    buf: &[u8],
) -> Result<Vec<SocketAddr>, Error> {
    let mut peer_list = Vec::<SocketAddr>::with_capacity(50);

    // in ipv4 the addresses come in packets of 6 bytes,
    // first 4 for ip and 2 for port
    // in ipv6 its 16 bytes for port and 2 for port
    let stride = if is_ipv6 { 18 } else { 6 };

    let chunks = buf.chunks_exact(stride);
    if !chunks.remainder().is_empty() {
        return Err(Error::CompactPeerListRemainder);
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

        let port = u16::from_be_bytes(
            port.try_into().expect("iterator guarantees bounds are OK"),
        );

        peer_list.push((ip, port).into());
    }
    let peers: Vec<SocketAddr> = peer_list.into_iter().collect();
    Ok(peers)
}
