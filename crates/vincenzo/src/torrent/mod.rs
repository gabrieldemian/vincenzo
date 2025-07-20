//! Torrent that is spawned by the Daemon
//!
//! A torrent will manage multiple peers, peers can send messages to the torrent
//! using [`TorrentMsg`], and torrent can send messages to the Peers using
//! [`PeerMsg`].

mod types;

// re-exports
pub use types::*;

use crate::{
    bitfield::Bitfield,
    daemon::{DaemonCtx, DaemonMsg},
    disk::DiskMsg,
    error::Error,
    magnet::Magnet,
    metainfo::Info,
    peer::{self, Direction, Peer, PeerCtx, PeerId, PeerMsg},
    tracker::{event::Event, Tracker, TrackerCtx, TrackerMsg, TrackerTrait},
};
use bendy::decoding::FromBencode;
use bitvec::{bitvec, prelude::Msb0};
use std::{
    collections::BTreeMap,
    net::SocketAddr,
    sync::{atomic::Ordering, Arc},
    time::Duration,
};
use tokio::{
    net::{TcpListener, TcpStream},
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
    pub info_pieces: BTreeMap<u32, Vec<u8>>,

    /// How much of the info was downloaded.
    pub downloaded_info_bytes: u32,

    /// Idle peers returned from an announce request to the tracker.
    /// Will be removed from this vec as we connect with them, and added as we
    /// request more peers to the tracker.
    pub idle_peers: Vec<SocketAddr>,

    /// Idle peers being handshaked and soon moved to `connected_peer`.
    pub connecting_peers: Vec<SocketAddr>,

    /// Connected peers, removed from `peers`.
    pub connected_peers: Vec<Arc<PeerCtx>>,

    pub error_peers: Vec<Peer<peer::PeerError>>,

    // The downloaded bytes of the previous second,
    // used to get the download rate in seconds.
    // this will be mutated on the frontend event loop.
    pub last_second_downloaded: u64,

    /// How fast the client is downloading this torrent.
    pub download_rate: u64,

    /// The total size of the torrent files, in bytes,
    /// this is a cache of ctx.info.get_size()
    pub size: u64,

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
        let info = RwLock::new(Info::default().name(name.clone()));

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
    #[tracing::instrument(skip(self), name = "torrent::start_and_run")]
    pub async fn start_and_run(
        self,
        listen: Option<SocketAddr>,
    ) -> Result<(), Error> {
        let mut torrent = self.start(listen).await?;

        torrent.spawn_outbound_peers().await?;
        torrent.spawn_inbound_peers().await?;
        torrent.run().await?;

        Ok(())
    }

    /// Start the Torrent, by sending `connect` and `announce_exchange`
    /// messages to one of the trackers, and returning a list of peers.
    /// But it doesn't run the torrent event loop.
    #[tracing::instrument(skip(self), name = "torrent::start")]
    pub async fn start(
        self,
        listen: Option<SocketAddr>,
    ) -> Result<Torrent<Connected>, Error> {
        let c = self.ctx.clone();
        let _org_trackers = c.magnet.organize_trackers();

        let mut tracker = Tracker::connect_to_tracker(
            self.ctx.magnet.parse_trackers(),
            self.ctx.info_hash.clone(),
        )
        .await?;

        let (res, payload) = tracker.announce(Event::Started).await?;

        let peers = tracker.parse_compact_peer_list(payload.as_ref())?;

        let stats = Stats {
            interval: res.interval,
            seeders: res.seeders,
            leechers: res.leechers,
        };

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
                connecting_peers: Vec::new(),
                error_peers: Vec::new(),
                downloaded_info_bytes: 0,
                bitfield: Bitfield::default(),
                stats,
                idle_peers: peers,
                tracker_ctx,
                size: 0,
                connected_peers: Vec::new(),
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
    #[tracing::instrument(skip_all, name = "torrent::start_outbound_peers")]
    pub async fn spawn_outbound_peers(&self) -> Result<(), Error> {
        let (otx, orx) = oneshot::channel();
        self.daemon_ctx.tx.send(DaemonMsg::GetConnectedPeers(otx)).await?;

        let daemon_connected_peers = orx.await?;
        let max_global_peers = self.daemon_ctx.config.max_global_peers;
        let max_torrent_peers = self.daemon_ctx.config.max_torrent_peers;

        // connecting peers will (probably) soon be connected, so we count them
        // too
        let currently_active = self.state.connected_peers.len()
            + self.state.connecting_peers.len();

        if currently_active == max_torrent_peers as usize
            || daemon_connected_peers == max_global_peers
        {
            return Ok(());
        }

        let to_request = max_torrent_peers as usize - currently_active;

        let ctx = self.ctx.clone();

        for peer in self.state.idle_peers.iter().take(to_request).cloned() {
            let local_peer_id = self.state.tracker_ctx.peer_id.clone();
            let ctx = ctx.clone();

            // send connections too other peers
            spawn(async move {
                match TcpStream::connect(peer).await {
                    Ok(socket) => {
                        Self::start_and_run_peer(
                            ctx.clone(),
                            socket,
                            local_peer_id,
                            Direction::Outbound,
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

    /// Spawn a new event loop every time a peer connect with us.
    #[tracing::instrument(skip(self))]
    pub async fn spawn_inbound_peers(&self) -> Result<(), Error> {
        debug!("accepting requests in {:?}", self.state.tracker_ctx.local_addr);

        let local_peer_socket =
            TcpListener::bind(self.state.tracker_ctx.local_addr).await?;
        let local_peer_id = self.state.tracker_ctx.peer_id.clone();
        let ctx = self.ctx.clone();

        // accept connections from other peers
        spawn(async move {
            debug!("accepting requests in {local_peer_socket:?}");

            loop {
                let local_peer_id = local_peer_id.clone();

                if let Ok((socket, addr)) = local_peer_socket.accept().await {
                    info!("received inbound connection from {addr}");
                    let ctx = ctx.clone();

                    spawn(async move {
                        Self::start_and_run_peer(
                            ctx,
                            socket,
                            local_peer_id,
                            Direction::Inbound,
                        )
                        .await?;

                        Ok::<(), Error>(())
                    });
                }
            }
        });

        Ok(())
    }

    #[tracing::instrument(skip_all, name = "torrent::start_and_run_peer")]
    async fn start_and_run_peer(
        ctx: Arc<TorrentCtx>,
        socket: TcpStream,
        local_peer_id: PeerId,
        direction: Direction,
    ) -> Result<Peer<peer::Connected>, Error> {
        let addr = socket.peer_addr()?;
        let torrent_tx = ctx.tx.clone();

        let idle_peer = Peer::<peer::Idle>::new(
            direction,
            ctx.info_hash.clone(),
            local_peer_id,
        );

        let connecting_peer = idle_peer.handshake(socket, ctx).await;

        match connecting_peer {
            Err(r) => {
                debug!("failed to handshake peer: {}", r);
                let _ = torrent_tx
                    .send(TorrentMsg::PeerConnectingError(addr))
                    .await;
                Err(r)
            }
            Ok(mut connected_peer) => {
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
        }
    }

    /// Get the best n downloaders.
    pub fn get_best_downloaders(&self, n: usize) -> Vec<Arc<PeerCtx>> {
        self.sort_peers_by_rate(n, false, true)
    }

    /// Get the worst n downloaders.
    pub fn get_worst_downloaders(&self, n: usize) -> Vec<Arc<PeerCtx>> {
        self.sort_peers_by_rate(n, false, false)
    }

    /// Get the best n uploaders.
    pub fn get_best_uploaders(&self, n: usize) -> Vec<Arc<PeerCtx>> {
        self.sort_peers_by_rate(n, true, true)
    }

    /// Get the worst n uploaders.
    pub fn get_worst_uploaders(&self, n: usize) -> Vec<Arc<PeerCtx>> {
        self.sort_peers_by_rate(n, true, false)
    }

    /// A fast function for returning the best or worst X amount of peers,
    /// uploaded or downloaded.
    /// - Doesn't allocate during sorting, only at the end of the function.
    /// - Cache friendly
    /// - Single pass through each peer, only 1 atomic read, on x86 a relaxed
    ///   read is just a add/mov so no performance impact.
    fn sort_peers_by_rate(
        &self,
        n: usize,
        get_uploaded: bool,
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

        // extract results (only n clones performed)
        buffer[..n].iter().map(|&(_, idx)| peers[idx].clone()).collect()
    }

    /// Run the Torrent main event loop to listen to internal [`TorrentMsg`].
    #[tracing::instrument(skip_all)]
    pub async fn run(&mut self) -> Result<(), Error> {
        debug!("running torrent: {:?}", self.name);

        let tracker_tx = &self.state.tracker_ctx.tx;
        let now = Instant::now();

        let mut announce_interval = interval_at(
            now + Duration::from_secs(
                self.state.stats.interval.max(500).into(),
            ),
            Duration::from_secs((self.state.stats.interval as u64).max(500)),
        );

        // unchoke a peer with the fewest pieces and choke the slowest
        // unchoked peer.
        let mut optimistic_unchoke_interval = interval(Duration::from_secs(30));

        // send state to the frontend, if connected.
        let mut heartbeat_interval = interval(Duration::from_secs(1));

        // choke algorithm:
        // - choose the top 4 interested uploaders and unchoke them.
        // - unchoke uninterested peers with a better upload rate, and if one
        //   becomes interested later, choke the worst uploader.
        let mut unchoke_interval = interval(Duration::from_secs(10));

        loop {
            select! {
                Some(msg) = self.rx.recv() => {
                    match msg {
                        TorrentMsg::ReadBitfield(oneshot) => {
                            let _ = oneshot.send(self.state.bitfield.clone());
                        }
                        TorrentMsg::SetBitfield(index) => {
                            self.state.bitfield.set(index, true);
                        }
                        TorrentMsg::DownloadedPiece(piece) => {
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
                            info!("Downloaded torrent {:?}", self.name);
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
                        TorrentMsg::StartEndgame(_peer_id, block_infos) => {
                            info!("Started endgame mode for {}", self.name);
                            for peer in &self.state.connected_peers {
                                let _ = peer.tx.send(PeerMsg::RequestBlockInfos(block_infos.clone())).await;
                            }
                        }
                        TorrentMsg::DownloadedInfoPiece(total, index, bytes) => {
                            debug!("received DownloadedInfoPiece");

                            if self.status == TorrentStatus::ConnectingTrackers {
                                self.status = TorrentStatus::DownloadingMetainfo;
                            }

                            self.state.downloaded_info_bytes += bytes.len() as u32;
                            self.state.info_pieces.insert(index, bytes);

                            let have_all_pieces = self.state.downloaded_info_bytes >= total;

                            if have_all_pieces {
                                // info has a valid bencode format
                                let info_bytes = self.state.info_pieces.values().fold(Vec::new(), |mut acc, b| {
                                    acc.extend_from_slice(b);
                                    acc
                                });
                                let info = Info::from_bencode(&info_bytes)?;

                                let m_info = self.ctx.magnet.hash().unwrap();

                                let mut hash = sha1_smol::Sha1::new();
                                hash.update(&info_bytes);

                                let hash = hash.digest().bytes();

                                // validate the hash of the downloaded info
                                // against the hash of the magnet link
                                let hash = hex::encode(hash);

                                if hash.to_uppercase() == m_info.to_uppercase() {
                                    debug!("the hash of the downloaded info matches the hash of the magnet link");

                                    // with the info fully downloaded, we now know the pieces len,
                                    // this will update the bitfield of the torrent
                                    self.state.bitfield = bitvec![u8, Msb0; 0; info.pieces() as usize];

                                    // remove excess bits
                                    if (info.pieces() as usize) < self.state.bitfield.len() {
                                        unsafe {
                                            self.state.bitfield.set_len(info.pieces() as usize);
                                        }
                                    }

                                    debug!("local_bitfield is now of len {:?}", self.state.bitfield.len());

                                    self.state.size = info.get_size();
                                    self.state.have_info = true;
                                    self.state.tracker_ctx.tx.send(TrackerMsg::Info(info.clone())).await?;

                                    let mut info_l = self.ctx.info.write().await;

                                    debug!("new info piece length {:?}", info.piece_length);
                                    debug!("new info pieces_len {:?}", info.pieces.len());
                                    debug!("new info pieces_len {:?}", info.pieces.len());
                                    debug!("new info file_length {:?}", info.file_length);
                                    debug!("new info files {:#?}", info.files);

                                    *info_l = info;
                                    drop(info_l);

                                    self.status = TorrentStatus::Downloading;
                                    self.ctx.disk_tx.send(DiskMsg::NewTorrent(self.ctx.clone())).await?;
                                } else {
                                    warn!("a peer sent a valid Info, but the hash does not match the hash of the provided magnet link, panicking");
                                    return Err(Error::PieceInvalid);
                                }
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
                            let is_download_complete = self.state.tracker_ctx.downloaded >= self.state.size;
                            debug!("IncrementDownloaded {:?}", self.state.tracker_ctx.downloaded);
                            debug!("size is {}", self.state.size);

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
                                    if self.state.tracker_ctx.downloaded >= self.state.size {
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
                            info!("Quitting torrent {:?}", self.name);
                            let tracker_tx = &self.state.tracker_ctx.tx;

                            let _ = tracker_tx.send(
                                TrackerMsg::Announce {
                                    event: Event::Stopped,
                                    recipient: None,
                                })
                            .await;

                            for peer in &self.state.connected_peers {
                                let tx = peer.tx.clone();
                                spawn(async move {
                                    let _ = tx.send(PeerMsg::Quit).await;
                                });
                            }

                            return Ok(());
                        }
                    }
                }
                _ = heartbeat_interval.tick() => {
                    self.state.download_rate =
                            self.state.tracker_ctx.downloaded - self.state.last_second_downloaded;

                    let torrent_state = TorrentState {
                        name: self.name.clone(),
                        size: self.state.size,
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
                    // debug!(
                    //     "{} {} of {}. Download rate: {}",
                    //     self.name,
                    //     to_human_readable(self.size as f64),
                    //     to_human_readable(self.downloaded as f64),
                    //     to_human_readable(self.download_rate as f64),
                    // );

                    // send updated information to daemon
                    let _ = self.daemon_ctx.tx.send(DaemonMsg::TorrentState(torrent_state)).await;
                }
                // periodically announce to tracker, at the specified interval
                // to update the tracker about the client's stats.
                _ = announce_interval.tick() => {
                    if self.state.have_info {
                        debug!("sending periodic announce, interval {announce_interval:?}");

                        let (otx, orx) = oneshot::channel();
                        let tracker_tx = &self.state.tracker_ctx.tx;

                        let _ = tracker_tx.send(
                            TrackerMsg::Announce {
                                event: Event::None,
                                recipient: Some(otx),
                            })
                        .await;

                        let r = orx.await?;
                        debug!("new stats {r:#?}");

                        // update our stats, received from the tracker
                        self.state.stats = r.0.into();

                        announce_interval = interval(
                            Duration::from_secs(self.state.stats.interval as u64),
                        );
                    }
                }
                _ = unchoke_interval.tick() => {
                    // todo
                }
                _ = optimistic_unchoke_interval.tick() => {
                    // todo
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
