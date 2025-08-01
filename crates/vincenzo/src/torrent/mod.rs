//! Torrent that is spawned by the Daemonpeer_choking
//!
//! A torrent will manage multiple peers, peers can send messages to the torrent
//! using [`TorrentMsg`], and torrent can send messages to the Peers using
//! [`PeerMsg`].

use bendy::decoding::FromBencode;
use bitvec::{bitvec, prelude::Msb0};
mod types;

use rand::Rng;
// re-exports
pub use types::*;

use crate::{
    bitfield::Bitfield,
    config::CONFIG,
    daemon::{DaemonCtx, DaemonMsg},
    disk::DiskMsg,
    error::Error,
    magnet::Magnet,
    metainfo::Info,
    peer::{self, DirectionWithInfoHash, Peer, PeerCtx, PeerId, PeerMsg},
    tracker::{event::Event, Tracker, TrackerCtx, TrackerMsg, TrackerTrait},
};
use std::{
    collections::BTreeMap,
    net::SocketAddr,
    sync::{atomic::Ordering, Arc},
    time::Duration,
};
use tokio::{
    net::TcpStream,
    select, spawn,
    sync::{mpsc, oneshot, RwLock},
    time::{interval, interval_at, Instant},
};
use tracing::{debug, info, warn};

pub trait TorrentTrait {}

// States of the torrent, idle is when the tracker is not connected and the
// torrent is not being downloaded
pub struct Idle;

pub struct Connected {
    /// Stats of the current Torrent, returned from tracker on announce
    /// requests.
    pub stats: Stats,

    /// Bitfield representing the presence or absence of pieces for our local
    /// peer, where each bit is a piece.
    pub bitfield: Bitfield,

    /// If the torrent has the full downloaded info (metadata) or not.
    pub have_info: bool,

    /// If using a Magnet link, the info will be downloaded in pieces
    /// and those pieces may come in different order,
    /// After it is complete, it will be encoded into [`Info`]
    pub info_pieces: BTreeMap<u64, Vec<u8>>,

    /// How much of the info was downloaded.
    pub downloaded_info_bytes: u64,

    /// Idle peers returned from an announce request to the tracker.
    /// Will be removed from this vec as we connect with them, and added as we
    /// request more peers to the tracker.
    pub idle_peers: Vec<SocketAddr>,

    /// Idle peers being handshaked and soon moved to `connected_peer`.
    pub connecting_peers: Vec<SocketAddr>,

    /// Connected peers, removed from `peers`.
    pub connected_peers: Vec<Arc<PeerCtx>>,

    /// Maximum of 3 unchoked peers as per the protocol + the optimistically
    /// unchoked peer = 4. These come from `connected_peers`.
    pub unchoked_peers: Vec<Arc<PeerCtx>>,

    /// Only one optimistically unchoked peer for 30 seconds.
    pub opt_unchoked_peer: Option<Arc<PeerCtx>>,

    pub error_peers: Vec<Peer<peer::PeerError>>,

    // The downloaded bytes of the previous second,
    // used to get the download rate in seconds.
    // this will be mutated on the frontend event loop.
    pub last_second_downloaded: u64,

    /// How fast the client is downloading this torrent.
    pub download_rate: u64,

    /// Size of the `info` bencoded string.
    pub metadata_size: u64,

    pub tracker_ctx: Arc<TrackerCtx>,
}

impl TorrentTrait for Idle {}
impl TorrentTrait for Connected {}

/// This is the main entity responsible for the high-level management of
/// a torrent download or upload.
pub struct Torrent<S: TorrentTrait> {
    pub ctx: Arc<TorrentCtx>,
    pub daemon_ctx: Arc<DaemonCtx>,
    pub name: String,
    pub rx: mpsc::Receiver<TorrentMsg>,
    pub state: S,
    pub status: TorrentStatus,
}

/// Context of [`Torrent`] that can be shared between other types
#[derive(Debug)]
pub struct TorrentCtx {
    pub disk_tx: mpsc::Sender<DiskMsg>,
    pub tx: mpsc::Sender<TorrentMsg>,
    pub magnet: Magnet,
    pub info_hash: InfoHash,
    pub info: RwLock<Info>,
}

impl Torrent<Idle> {
    #[tracing::instrument(skip(disk_tx, daemon_ctx), name = "torrent::new")]
    pub fn new(
        disk_tx: mpsc::Sender<DiskMsg>,
        daemon_ctx: Arc<DaemonCtx>,
        magnet: Magnet,
    ) -> Torrent<Idle> {
        let name = magnet.parse_dn();

        let mut info = Info::default().name(name.clone());

        if let Some(size) = magnet.length() {
            info.metadata_size = Some(size);
        }

        let info = RwLock::new(info);

        let (tx, rx) = mpsc::channel::<TorrentMsg>(100);

        let ctx = Arc::new(TorrentCtx {
            tx: tx.clone(),
            disk_tx,
            info_hash: magnet.parse_xt_infohash(),
            magnet,
            info,
        });

        Self {
            state: Idle,
            name,
            status: TorrentStatus::default(),
            daemon_ctx,
            ctx,
            rx,
        }
    }

    /// Start the Torrent and immediately spawns all the event loops.
    #[tracing::instrument(skip_all)]
    pub async fn start_and_run(self) -> Result<(), Error> {
        let mut torrent = self.start().await?;

        torrent.spawn_outbound_peers().await?;
        torrent.run().await?;

        Ok(())
    }

    /// Start the Torrent, by sending `connect` and `announce_exchange`
    /// messages to one of the trackers, and returning a list of peers.
    /// But it doesn't run the torrent event loop.
    #[tracing::instrument(skip_all)]
    pub async fn start(self) -> Result<Torrent<Connected>, Error> {
        let c = self.ctx.clone();
        let org_trackers = c.magnet.organize_trackers();

        let udp_trackers = org_trackers.get("udp").unwrap();

        if udp_trackers.is_empty() {
            return Err(Error::MagnetNoTracker);
        }

        let mut tracker = Tracker::connect_to_tracker(
            udp_trackers,
            self.ctx.info_hash.clone(),
            self.daemon_ctx.clone(),
        )
        .await?;

        info!(
            "connected to tracker: {:?}, sending announce",
            tracker.ctx.tracker_addr
        );

        let (res, payload) = tracker.announce(Event::Started).await?;
        let peers = tracker.parse_compact_peer_list(payload.as_ref())?;

        info!("announced and got {} peers", peers.len());

        let stats = Stats {
            interval: res.interval,
            seeders: res.seeders,
            leechers: res.leechers,
        };

        info!("{stats:?}");

        debug!(
            "starting torrent {:?} with stats {:#?}",
            self.ctx.info_hash, stats
        );

        let tracker_ctx = Arc::new(tracker.ctx.clone());

        spawn(async move {
            tracker.run().await?;
            Ok::<(), Error>(())
        });

        Ok(Torrent {
            state: Connected {
                unchoked_peers: Vec::with_capacity(3),
                opt_unchoked_peer: None,
                connecting_peers: Vec::with_capacity(
                    CONFIG.max_torrent_peers as usize,
                ),
                error_peers: Vec::with_capacity(
                    CONFIG.max_torrent_peers as usize,
                ),
                downloaded_info_bytes: 0,
                bitfield: Bitfield::default(),
                stats,
                idle_peers: peers,
                tracker_ctx,
                metadata_size: self.ctx.magnet.length().unwrap_or(0),
                connected_peers: Vec::with_capacity(
                    CONFIG.max_torrent_peers as usize,
                ),
                have_info: false,
                info_pieces: BTreeMap::new(),
                download_rate: 0,
                last_second_downloaded: 0,
            },
            ctx: self.ctx,
            daemon_ctx: self.daemon_ctx,
            name: self.name,
            rx: self.rx,
            status: self.status,
        })
    }
}

impl Torrent<Connected> {
    /// Spawn an event loop for each peer
    pub async fn spawn_outbound_peers(&self) -> Result<(), Error> {
        let (otx, orx) = oneshot::channel();

        self.daemon_ctx.tx.send(DaemonMsg::GetConnectedPeers(otx)).await?;

        let daemon_connected_peers = orx.await?;
        let max_global_peers = CONFIG.max_global_peers;
        let max_torrent_peers = CONFIG.max_torrent_peers;

        // connecting peers will (probably) soon be connected, so we count them
        // too
        let currently_active = self.state.connected_peers.len()
            + self.state.connecting_peers.len();

        if currently_active >= max_torrent_peers as usize
            || daemon_connected_peers >= max_global_peers
        {
            return Ok(());
        }

        let to_request = max_torrent_peers as usize - currently_active;

        let ctx = self.ctx.clone();
        let daemon_ctx = self.daemon_ctx.clone();

        if to_request > 0 {
            info!(
                "{:?} sending handshakes to {to_request} peers",
                self.ctx.info_hash
            );
        }

        for peer in self.state.idle_peers.iter().take(to_request).cloned() {
            let ctx = ctx.clone();
            let daemon_ctx = daemon_ctx.clone();

            // send connections too other peers
            spawn(async move {
                match TcpStream::connect(peer).await {
                    Ok(socket) => {
                        Self::start_and_run_peer(
                            daemon_ctx.clone(),
                            socket,
                            DirectionWithInfoHash::Outbound(
                                ctx.info_hash.clone(),
                            ),
                        )
                        .await?;
                    }
                    Err(e) => {
                        debug!("error with peer: {:?} {e:#?}", peer);
                        ctx.tx.send(TorrentMsg::PeerError(peer)).await?;
                    }
                }
                Ok::<(), Error>(())
            });
        }

        Ok(())
    }

    pub async fn start_and_run_peer(
        daemon_ctx: Arc<DaemonCtx>,
        socket: TcpStream,
        direction: DirectionWithInfoHash,
    ) -> Result<Peer<peer::Connected>, Error> {
        let idle_peer = Peer::<peer::Idle>::new();

        let mut connected_peer =
            idle_peer.handshake(socket, daemon_ctx, direction).await?;

        if let Err(r) = connected_peer.run().await {
            debug!(
                "{} Peer session stopped due to an error: {}",
                connected_peer.state.ctx.remote_addr, r
            );
            connected_peer.free_pending_blocks().await;
            return Err(r);
        }
        Ok(connected_peer)
    }

    /// Get the best n downloaders.
    pub fn get_best_downloaders(&self, n: usize) -> Vec<Arc<PeerCtx>> {
        self.sort_peers_by_rate(n, false, false, true)
    }

    /// Get the best n downloaders that are interested in the client.
    pub fn get_best_interested_downloaders(
        &self,
        n: usize,
    ) -> Vec<Arc<PeerCtx>> {
        self.sort_peers_by_rate(n, false, true, true)
    }

    /// Get the best n uploaders that are interested in the client.
    pub fn get_best_interested_uploaders(&self, n: usize) -> Vec<Arc<PeerCtx>> {
        self.sort_peers_by_rate(n, true, true, true)
    }

    /// Get the worst n downloaders.
    pub fn get_worst_downloaders(&self, n: usize) -> Vec<Arc<PeerCtx>> {
        self.sort_peers_by_rate(n, false, false, false)
    }

    /// Get the worst n downloaders that are interested in the client.
    pub fn get_worst_interested_downloaders(
        &self,
        n: usize,
    ) -> Vec<Arc<PeerCtx>> {
        self.sort_peers_by_rate(n, false, true, false)
    }

    /// Get the best n uploaders.
    pub fn get_best_uploaders(&self, n: usize) -> Vec<Arc<PeerCtx>> {
        self.sort_peers_by_rate(n, true, false, true)
    }

    /// Get the worst n uploaders.
    pub fn get_worst_uploaders(&self, n: usize) -> Vec<Arc<PeerCtx>> {
        self.sort_peers_by_rate(n, true, false, false)
    }

    pub fn get_next_opt_unchoked_peer(&self) -> Option<Arc<PeerCtx>> {
        let mut min = u64::MAX;
        let mut result = None;

        for peer in &self.state.connected_peers {
            let peer_uploaded = peer.uploaded.load(Ordering::Relaxed);

            match &self.state.opt_unchoked_peer {
                Some(opt_unchoked) => {
                    if peer_uploaded < min && opt_unchoked.id != peer.id {
                        min = peer_uploaded;
                        result = Some(opt_unchoked.clone());
                    }
                }
                None => {
                    if peer_uploaded < min {
                        min = peer_uploaded;
                        result = Some(peer.clone());
                    }
                }
            }
        }

        result
    }

    /// A fast function for returning the best or worst N amount of peers,
    /// uploaded or downloaded. Note that the maximum value of N is 10.
    /// - Doesn't allocate during sorting, only at the end of the function.
    /// - Cache friendly
    /// - Single pass through each peer with only 1 atomic read, on x86 a
    ///   relaxed read is just a add/mov so no performance impact.
    fn sort_peers_by_rate(
        &self,
        n: usize,
        get_uploaded: bool,
        skip_uninterested: bool,
        is_asc: bool,
    ) -> Vec<Arc<PeerCtx>> {
        let peers = &self.state.connected_peers;

        if n == 0 || peers.is_empty() {
            return Vec::new();
        }

        // constrain n to min(10, peers.len())
        let n = n.min(peers.len()).min(10);
        let mut buffer = [(u64::MIN, usize::MIN); 10];
        let mut len = 0;

        for (index, peer) in peers.iter().enumerate() {
            if skip_uninterested
                && !peer.peer_interested.load(Ordering::Relaxed)
            {
                continue;
            }

            let uploaded_or_downloaded = if get_uploaded {
                peer.uploaded.load(Ordering::Relaxed)
            } else {
                peer.downloaded.load(Ordering::Relaxed)
            };

            if len < n {
                // insert new element
                buffer[len] = (uploaded_or_downloaded, index);
                let mut pos = len;

                // bubble up to maintain order
                while pos > 0
                    && if is_asc {
                        buffer[pos].0 > buffer[pos - 1].0
                    } else {
                        buffer[pos].0 < buffer[pos - 1].0
                    }
                {
                    buffer.swap(pos, pos - 1);
                    pos -= 1;
                }
                len += 1;
            } else if if is_asc {
                uploaded_or_downloaded > buffer[n - 1].0
            } else {
                uploaded_or_downloaded < buffer[n - 1].0
            } {
                // replace smallest element in top list
                buffer[n - 1] = (uploaded_or_downloaded, index);
                let mut pos = n - 1;

                // bubble up to maintain descending order
                while pos > 0 && buffer[pos].0 > buffer[pos - 1].0 {
                    buffer.swap(pos, pos - 1);
                    pos -= 1;
                }
            }
        }

        buffer[..n].iter().map(|&(_, idx)| peers[idx].clone()).collect()
    }

    /// Become interested in the connected peer.
    pub async fn interested_peer(&self, id: PeerId) -> Result<(), Error> {
        if let Some(ctx) =
            self.state.connected_peers.iter().find(|v| v.id == id)
        {
            ctx.tx.send(PeerMsg::Interested).await?;
            return Ok(());
        }
        Err(Error::PeerNotFound(id))
    }

    /// Become disinterested in the connected peer.
    pub async fn disinterested_peer(&self, id: PeerId) -> Result<(), Error> {
        if let Some(ctx) =
            self.state.connected_peers.iter().find(|v| v.id == id)
        {
            ctx.tx.send(PeerMsg::NotInterested).await?;
            return Ok(());
        }
        Err(Error::PeerNotFound(id))
    }

    /// Unchoke a connected peer and update local state.
    pub async fn unchoke_peer(&mut self, id: PeerId) -> Result<(), Error> {
        if self.state.unchoked_peers.len() >= 3 {
            return Err(Error::MaximumUnchokedPeers);
        }
        if let Some(ctx) =
            self.state.connected_peers.iter().find(|v| v.id == id)
        {
            ctx.tx.send(PeerMsg::Unchoke).await?;
            self.state.unchoked_peers.push(ctx.clone());
            return Ok(());
        }
        Err(Error::PeerNotFound(id))
    }

    fn get_random_choked_interested_peer(&self) -> Option<Arc<PeerCtx>> {
        let mut rng = rand::rng();
        let mut candidates = Vec::new();

        for peer in &self.state.connected_peers {
            // Skip already unchoked peers (regular or optimistic)
            if self.state.unchoked_peers.iter().any(|p| p.id == peer.id)
                || self.state.opt_unchoked_peer.as_ref().map(|p| &p.id)
                    == Some(&peer.id)
            {
                continue;
            }

            // Only consider interested peers
            if peer.peer_interested.load(Ordering::Relaxed) {
                candidates.push(peer.clone());
            }
        }

        if candidates.is_empty() {
            None
        } else {
            let idx = rng.random_range(0..candidates.len());
            Some(candidates[idx].clone())
        }
    }

    /// Choke a connected peer and update local state without doing any
    /// checks.
    pub async fn choke_peer(&mut self, id: PeerId) -> Result<(), Error> {
        if let Some(ctx) = self.state.unchoked_peers.iter().find(|v| v.id == id)
        {
            ctx.tx.send(PeerMsg::Choke).await?;
            self.state.unchoked_peers.retain(|v| v.id != id);
            return Ok(());
        }
        Err(Error::PeerNotFound(id))
    }

    /// Optimistically unchoke a connected peer
    pub async fn opt_unchoke_peer(&mut self, id: PeerId) -> Result<(), Error> {
        if let Some(ctx) =
            self.state.connected_peers.iter().find(|v| v.id == id)
        {
            ctx.tx.send(PeerMsg::Unchoke).await?;
            self.state.opt_unchoked_peer = Some(ctx.clone());
            return Ok(());
        }
        Err(Error::PeerNotFound(id))
    }

    /// Run the Torrent main event loop to listen to internal [`TorrentMsg`].
    #[tracing::instrument(skip_all)]
    pub async fn run(&mut self) -> Result<(), Error> {
        debug!("running torrent: {:?}", self.name);

        let tracker_tx = &self.state.tracker_ctx.tx;
        let now = Instant::now();

        // send feedback to the tracker
        let mut announce_interval = interval_at(
            now + Duration::from_secs(self.state.stats.interval.into()),
            Duration::from_secs(self.state.stats.interval.into()),
        );

        // try to reconnect with errored peers
        let mut reconnect_interval = interval(Duration::from_secs(5));

        // send state to the frontend, if connected.
        let mut heartbeat_interval = interval(Duration::from_secs(1));

        // unchoke the slowest interested peer.
        let mut optimistic_unchoke_interval = interval(Duration::from_secs(30));

        // unchoke algorithm:
        // - choose the best 3 interested uploaders and unchoke them.
        let mut unchoke_interval =
            interval_at(now + Duration::from_secs(10), Duration::from_secs(10));

        loop {
            select! {
                Some(msg) = self.rx.recv() => {
                    match msg {
                        TorrentMsg::ReadPeerByIp(ip, port, otx) => {
                            if let Some(peer_ctx) =
                                self.state.connected_peers
                                .iter()
                                .find(|&p| p.remote_addr.ip() == ip && port == p.remote_addr.port()) {
                                let _ = otx.send(Some(peer_ctx.clone()));
                                continue;
                            }
                            let _ = otx.send(None);
                        }
                        TorrentMsg::MetadataSize(meta_size) => {
                            if self.state.have_info {continue};

                            let mut info = self.ctx.info.write().await;
                            info.metadata_size = Some(meta_size);
                            self.state.metadata_size = meta_size;
                        }
                        TorrentMsg::ReadBitfield(oneshot) => {
                            let _ = oneshot.send(self.state.bitfield.clone());
                        }
                        TorrentMsg::SetBitfield(index) => {
                            self.state.bitfield.set(index, true);
                        }
                        TorrentMsg::DownloadedPiece(piece) => {
                            info!("downloaded_piece {piece}");
                            // send Have messages to peers that dont have our pieces
                            for peer in &self.state.connected_peers {
                                let _ = peer.tx.send(PeerMsg::HavePiece(piece)).await;
                            }
                        }
                        TorrentMsg::PeerConnecting(addr) => {
                            self.state.idle_peers.retain(|v| *v != addr);
                            self.state.connecting_peers.push(addr);
                        }
                        TorrentMsg::PeerConnectingError(addr) => {
                            self.state.connecting_peers.retain(|v| *v != addr);
                            // we dont push this addr to the error_peers. If the TCP connection was
                            // made but something happened in the handshake, there is nothing we
                            // can do but to ignore this peer's existence.
                        }
                        TorrentMsg::PeerError(addr) => {
                            self.state.error_peers.push(Peer::<peer::PeerError>::new(addr));
                            self.state.connected_peers.retain(|v| v.remote_addr != addr);
                            self.state.unchoked_peers.retain(|v| v.remote_addr != addr);

                            if let Some(opt_addr) =
                                self.state.opt_unchoked_peer.as_ref().map(|v| v.remote_addr)
                            {
                                if opt_addr == addr {
                                    self.state.opt_unchoked_peer = None;
                                }

                            }
                            self.state.idle_peers.retain(|v| *v != addr);

                            let _ = self
                                .ctx
                                .disk_tx
                                .send(DiskMsg::DeletePeer(addr))
                                .await;

                            let _ = self
                                .daemon_ctx
                                .tx
                                .send(DaemonMsg::DecrementConnectedPeers)
                                .await;
                        },
                        TorrentMsg::PeerConnected(ctx) => {
                            debug!("{} connected with {}", ctx.local_addr, ctx.remote_addr);

                            self.state.connected_peers.push(ctx.clone());
                            self.state.connecting_peers.retain(|v| *v != ctx.remote_addr);
                            // in the case that the PeerConnected arrives after the PeerConnecting
                            self.state.idle_peers.retain(|v| *v != ctx.remote_addr);

                            let _ = self
                                .ctx
                                .disk_tx
                                .send(DiskMsg::NewPeer(ctx))
                                .await;

                            let _ = self
                                .daemon_ctx
                                .tx
                                .send(DaemonMsg::IncrementConnectedPeers)
                                .await;
                        }
                        TorrentMsg::DownloadComplete => {
                            info!("downloaded torrent {:?}", self.name);
                            let (otx, orx) = oneshot::channel();

                            self.status = TorrentStatus::Seeding;

                            let _ = tracker_tx.send(
                                TrackerMsg::Announce {
                                    event: Event::Completed,
                                    recipient: Some(otx),
                                })
                            .await;

                            if let Ok(r) = orx.await {
                                debug!("announced completion with success {r:#?}");
                                self.state.stats = r.0.into();
                            }

                            // tell all peers that we are not interested,
                            // we wont request blocks from them anymore
                            for peer in &self.state.connected_peers {
                                let _ = peer.tx.send(PeerMsg::NotInterested).await;
                                let _ = peer.tx.send(PeerMsg::SeedOnly).await;
                            }
                        }
                        // The peer "from" was the first one to receive the "info".
                        // Send Cancel messages to everyone else.
                        // todo: move to broadcast
                        TorrentMsg::SendCancelBlock { from, block_info } => {
                            for peer in &self.state.connected_peers {
                                if peer.id == from { continue };
                                let _ = peer.tx.send(PeerMsg::CancelBlock(block_info.clone())).await;
                            }
                        }
                        // todo: move to broadcast
                        TorrentMsg::StartEndgame(block_infos) => {
                            info!("started endgame mode for {}", self.name);

                            for peer in &self.state.connected_peers {
                                let _ = peer.tx.send(
                                    PeerMsg::RequestBlockInfos(block_infos.clone())
                                )
                                .await;
                            }
                        }
                        TorrentMsg::DownloadedInfoPiece(total, index, bytes) => {
                            debug!("received downloaded_info_piece");

                            if self.status == TorrentStatus::ConnectingTrackers {
                                self.status = TorrentStatus::DownloadingMetainfo;
                            }

                            self.state.downloaded_info_bytes += bytes.len() as u64;
                            self.state.info_pieces.insert(index, bytes);

                            let have_all_pieces = self.state.downloaded_info_bytes >= total;

                            if !have_all_pieces { continue };

                            info!("{:?} have all info_hash pieces", self.ctx.info_hash);

                            let info_bytes =
                                self.state.info_pieces
                                .values()
                                .fold(Vec::new(), |mut acc, b| {
                                    acc.extend_from_slice(b);
                                    acc
                                });

                            let downloaded_info = Info::from_bencode(&info_bytes)?;
                            let magnet_hash = self.ctx.magnet.hash().unwrap();

                            let mut hasher = sha1_smol::Sha1::new();
                            hasher.update(&info_bytes);

                            let hash = hasher.digest().bytes();

                            // validate the hash of the downloaded info
                            // against the hash of the magnet link
                            if
                                hex::decode(magnet_hash)
                                .map_err(|_| Error::BencodeError)?
                                != hash.to_vec()
                            {
                                warn!("invalid info hash for info: {:?}", downloaded_info.name);
                                self.state.info_pieces.clear();
                                return Err(Error::PieceInvalid);
                            }

                            info!("hash of info is valid");
                            info!("--info--");
                            info!("name: {:?}", downloaded_info.name);
                            info!("files: {:?}", downloaded_info.files);
                            info!("piece_length: {:?}", downloaded_info.piece_length);
                            info!("pieces: {:?}", downloaded_info.pieces());

                            let mut info = self.ctx.info.write().await;

                            *info = downloaded_info;

                            let meta_size = info.metadata_size()?;
                            info.metadata_size = Some(meta_size);
                            self.state.metadata_size = info.metadata_size()?;

                            let pieces = info.pieces();

                            self.state.bitfield = bitvec![u8, Msb0; 0; pieces as usize];

                            info!("local_bitfield len {:?}", self.state.bitfield.len());
                            debug!("local_bitfield {:?}", self.state.bitfield);

                            self.state.have_info = true;
                            self.state.tracker_ctx.tx.send(TrackerMsg::Info(info.clone())).await?;

                            self.status = TorrentStatus::Downloading;
                            self.ctx.disk_tx.send(DiskMsg::NewTorrent(self.ctx.clone())).await?;

                            for peer in &self.state.connected_peers {
                                let _ = peer.tx.send(PeerMsg::HaveInfo).await;
                            }
                        }
                        TorrentMsg::RequestInfoPiece(index, recipient) => {
                            debug!("received RequestInfoPiece {index}");
                            let bytes = self.state.info_pieces.get(&index).cloned();
                            let _ = recipient.send(bytes);
                        }
                        TorrentMsg::IncrementDownloaded(downloaded) => {
                            let tx = &self.state.tracker_ctx.tx;

                            tx.send(TrackerMsg::Increment { downloaded, uploaded: 0 }).await?;

                            // check if the torrent download is complete
                            let is_download_complete = self.state.tracker_ctx.downloaded >= self.state.metadata_size;
                            debug!("IncrementDownloaded {:?}", self.state.tracker_ctx.downloaded);
                            debug!("size is {}", self.state.metadata_size);

                            if is_download_complete {
                                self.ctx.tx.send(TorrentMsg::DownloadComplete).await?;
                            }
                        }
                        TorrentMsg::IncrementUploaded(uploaded) => {
                            let tracker_tx = &self.state.tracker_ctx.tx;
                            tracker_tx.send(TrackerMsg::Increment { downloaded: 0, uploaded }).await?;
                            debug!("IncrementUploaded {}", self.state.tracker_ctx.uploaded);
                        }
                        TorrentMsg::TogglePause => {
                            debug!("torrent TogglePause");
                            // can only pause if the torrent is not connecting, or not erroring
                            if self.status == TorrentStatus::Downloading || self.status == TorrentStatus::Seeding || self.status == TorrentStatus::Paused {
                                info!("Paused torrent {:?}", self.name);
                                if self.status == TorrentStatus::Paused {
                                    if self.state.tracker_ctx.downloaded >= self.state.metadata_size {
                                        self.status = TorrentStatus::Seeding;
                                    } else {
                                        self.status = TorrentStatus::Downloading;
                                    }
                                } else {
                                    self.status = TorrentStatus::Paused;
                                }
                                for peer in &self.state.connected_peers {
                                    if self.status == TorrentStatus::Paused {
                                        let _ = peer.tx.send(PeerMsg::Pause).await;
                                    } else {
                                        let _ = peer.tx.send(PeerMsg::Resume).await;
                                    }
                                }
                            }
                        }
                        TorrentMsg::Quit => {
                            info!("quitting torrent {:?}", self.name);
                            let tracker_tx = &self.state.tracker_ctx.tx;

                            for peer in &self.state.connected_peers {
                                let tx = peer.tx.clone();
                                spawn(async move {
                                    let _ = tx.send(PeerMsg::Quit).await;
                                });
                            }

                            let _ = tracker_tx.send(
                                TrackerMsg::Announce {
                                    event: Event::Stopped,
                                    recipient: None,
                                })
                            .await;

                            return Ok(());
                        }
                    }
                }
                _ = reconnect_interval.tick() => {
                    debug!("
                        reconnect_interval
                        connected_peers: {}
                        error_peers: {}
                    ",
                        self.state.connected_peers.len(),
                        self.state.error_peers.len(),
                    );

                    let errored: Vec<_> = self.
                        state.
                        error_peers
                        .drain(..)
                        .map(|v| v.state.addr).collect();

                    self.state.idle_peers.extend(errored);

                    self.spawn_outbound_peers().await?;
                }
                _ = heartbeat_interval.tick() => {

                    self.state.download_rate =
                            self.state.tracker_ctx.downloaded - self.state.last_second_downloaded;

                    let torrent_state = TorrentState {
                        name: self.name.clone(),
                        size: self.state.metadata_size,
                        downloaded: self.state.tracker_ctx.downloaded,
                        uploaded: self.state.tracker_ctx.uploaded,
                        stats: self.state.stats.clone(),
                        status: self.status.clone(),
                        download_rate: self.state.download_rate,
                        info_hash: self.ctx.info_hash.clone(),
                        have_info: self.state.have_info,
                        bitfield: self.state.bitfield.clone().into_vec(),
                        connected_peers: self.state.connected_peers.len() as u8,
                        connecting_peers: self.state.connecting_peers.len() as u8,
                        idle_peers: self.state.idle_peers.len() as u8,
                    };

                    self.state.last_second_downloaded = self.state.tracker_ctx.downloaded;

                    let _ = self.daemon_ctx.tx.send(DaemonMsg::TorrentState(torrent_state)).await;
                }
                _ = optimistic_unchoke_interval.tick() => {
                    if let Some(old_opt) = self.state.opt_unchoked_peer.take() {
                        // only choke if not in top 3
                        if !self.state.unchoked_peers.iter().any(|p| p.id == old_opt.id) {
                            let _ = old_opt.tx.send(PeerMsg::Choke).await;
                        }
                    }

                    // select new optimistic unchoke
                    if let Some(new_opt) = self.get_random_choked_interested_peer() {
                        info!("opt unchoking {:?}", new_opt.id);
                        let _ = new_opt.tx.send(PeerMsg::Unchoke).await;
                        self.state.opt_unchoked_peer = Some(new_opt);
                    }
                }
                // for the unchoke interval, the local client is interested in the best
                // uploaders (from our perspctive) (tit-for-tat) which gives us the most bytes out of the other
                _ = unchoke_interval.tick() => {
                    let best_uploaders = self.get_best_interested_uploaders(3);

                    // choke peers no longer in top 3
                    for peer in &self.state.unchoked_peers {
                        if !best_uploaders.iter().any(|p| p.id == peer.id) {
                            info!("choking peer {:?}", peer.id);
                            let _ = peer.tx.send(PeerMsg::Choke).await;
                        }
                    }

                    for uploader in &best_uploaders {
                        if !self.state.unchoked_peers.iter().any(|p| p.id == uploader.id) {
                            info!("unchoking peer {:?}", uploader.id);

                            let _ = uploader.tx.send(PeerMsg::Unchoke).await;
                            self.state.unchoked_peers.push(uploader.clone());
                        }
                    }

                    for peer in &self.state.connected_peers {
                        peer.downloaded.store(0, Ordering::Relaxed);
                        peer.uploaded.store(0, Ordering::Relaxed);
                    }
                }
                _ = announce_interval.tick(), if self.state.have_info => {
                    debug!("sending periodic announce, interval {announce_interval:?}");

                    let (otx, orx) = oneshot::channel();
                    let tracker_tx = &self.state.tracker_ctx.tx;

                    let _ = tracker_tx.send(
                        TrackerMsg::Announce {
                            event: Event::None,
                            recipient: Some(otx),
                        })
                    .await;

                    let (resp, _payload) = orx.await?;
                    debug!("new stats {resp:#?}");

                    // update our stats, received from the tracker
                    self.state.stats = resp.into();

                    announce_interval = interval(
                        Duration::from_secs(self.state.stats.interval as u64),
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    };

    #[test]
    fn test_get_top_uploaders() {
        #[derive(Debug)]
        struct PeerCtx {
            uploaded: AtomicU64,
            downloaded: AtomicU64,
        }
        fn get_top_uploaders(
            peers: Vec<Arc<PeerCtx>>,
            x: usize,
            is_uploaders: bool,
            is_asc: bool,
        ) -> Vec<Arc<PeerCtx>> {
            // Handle edge cases
            if x == 0 || peers.is_empty() {
                return Vec::new();
            }

            // Constrain x to min(10, peers.len())
            let x = x.min(peers.len()).min(10);
            let mut buffer = [(u64::MIN, usize::MIN); 10];
            let mut len = 0;

            for (index, peer) in peers.iter().enumerate() {
                // Relaxed ordering sufficient for snapshot value
                let uploaded_or_downloaded = if is_uploaders {
                    peer.uploaded.load(Ordering::Relaxed)
                } else {
                    peer.downloaded.load(Ordering::Relaxed)
                };

                if len < x {
                    // Insert new element
                    buffer[len] = (uploaded_or_downloaded, index);
                    let mut pos = len;

                    // Bubble up to maintain descending order
                    while pos > 0
                        && if is_asc {
                            buffer[pos].0 > buffer[pos - 1].0
                        } else {
                            buffer[pos].0 < buffer[pos - 1].0
                        }
                    {
                        buffer.swap(pos, pos - 1);
                        pos -= 1;
                    }
                    len += 1;
                } else if if is_asc {
                    uploaded_or_downloaded > buffer[x - 1].0
                } else {
                    uploaded_or_downloaded < buffer[x - 1].0
                } {
                    // Replace smallest element in top list
                    buffer[x - 1] = (uploaded_or_downloaded, index);
                    let mut pos = x - 1;

                    // Bubble up to maintain descending order
                    while pos > 0 && buffer[pos].0 > buffer[pos - 1].0 {
                        buffer.swap(pos, pos - 1);
                        pos -= 1;
                    }
                }
            }

            // Extract results (only x clones performed)
            buffer[..x].iter().map(|&(_, idx)| peers[idx].clone()).collect()
        }

        let r = get_top_uploaders(
            vec![
                Arc::new(PeerCtx { uploaded: 9.into(), downloaded: 0.into() }),
                Arc::new(PeerCtx { uploaded: 8.into(), downloaded: 0.into() }),
                Arc::new(PeerCtx { uploaded: 7.into(), downloaded: 0.into() }),
                Arc::new(PeerCtx { uploaded: 6.into(), downloaded: 0.into() }),
                Arc::new(PeerCtx { uploaded: 5.into(), downloaded: 0.into() }),
                Arc::new(PeerCtx { uploaded: 4.into(), downloaded: 0.into() }),
                Arc::new(PeerCtx { uploaded: 3.into(), downloaded: 0.into() }),
                Arc::new(PeerCtx { uploaded: 2.into(), downloaded: 0.into() }),
                Arc::new(PeerCtx { uploaded: 1.into(), downloaded: 0.into() }),
                Arc::new(PeerCtx { uploaded: 0.into(), downloaded: 0.into() }),
            ],
            4,
            true,
            true,
        );
        assert_eq!(r[0].uploaded.load(Ordering::Relaxed), 9);
        assert_eq!(r[1].uploaded.load(Ordering::Relaxed), 8);
        assert_eq!(r[2].uploaded.load(Ordering::Relaxed), 7);
        assert_eq!(r[3].uploaded.load(Ordering::Relaxed), 6);

        let r = get_top_uploaders(
            vec![
                Arc::new(PeerCtx { uploaded: 9.into(), downloaded: 1.into() }),
                Arc::new(PeerCtx { uploaded: 8.into(), downloaded: 2.into() }),
                Arc::new(PeerCtx { uploaded: 7.into(), downloaded: 3.into() }),
                Arc::new(PeerCtx { uploaded: 6.into(), downloaded: 4.into() }),
                Arc::new(PeerCtx { uploaded: 5.into(), downloaded: 5.into() }),
                Arc::new(PeerCtx { uploaded: 4.into(), downloaded: 9.into() }),
                Arc::new(PeerCtx { uploaded: 3.into(), downloaded: 8.into() }),
                Arc::new(PeerCtx { uploaded: 2.into(), downloaded: 7.into() }),
                Arc::new(PeerCtx { uploaded: 1.into(), downloaded: 6.into() }),
                Arc::new(PeerCtx { uploaded: 0.into(), downloaded: 5.into() }),
            ],
            4,
            false,
            true,
        );
        assert_eq!(r[0].downloaded.load(Ordering::Relaxed), 9);
        assert_eq!(r[1].downloaded.load(Ordering::Relaxed), 8);
        assert_eq!(r[2].downloaded.load(Ordering::Relaxed), 7);
        assert_eq!(r[3].downloaded.load(Ordering::Relaxed), 6);

        let r = get_top_uploaders(
            vec![
                Arc::new(PeerCtx { uploaded: 9.into(), downloaded: 1.into() }),
                Arc::new(PeerCtx { uploaded: 8.into(), downloaded: 2.into() }),
                Arc::new(PeerCtx { uploaded: 7.into(), downloaded: 3.into() }),
                Arc::new(PeerCtx { uploaded: 6.into(), downloaded: 4.into() }),
                Arc::new(PeerCtx { uploaded: 5.into(), downloaded: 5.into() }),
                Arc::new(PeerCtx { uploaded: 4.into(), downloaded: 9.into() }),
                Arc::new(PeerCtx { uploaded: 3.into(), downloaded: 8.into() }),
                Arc::new(PeerCtx { uploaded: 2.into(), downloaded: 7.into() }),
                Arc::new(PeerCtx { uploaded: 1.into(), downloaded: 6.into() }),
                Arc::new(PeerCtx { uploaded: 0.into(), downloaded: 5.into() }),
            ],
            4,
            false,
            false,
        );
        assert_eq!(r[0].downloaded.load(Ordering::Relaxed), 1);
        assert_eq!(r[1].downloaded.load(Ordering::Relaxed), 2);
        assert_eq!(r[2].downloaded.load(Ordering::Relaxed), 3);
        assert_eq!(r[3].downloaded.load(Ordering::Relaxed), 4);
    }
}
