//! Torrent that is spawned by the Daemonpeer_choking
//!
//! A torrent will manage multiple peers, peers can send messages to the torrent
//! using [`TorrentMsg`], and torrent can send messages to the Peers using
//! [`PeerMsg`].

mod from_magnet;
mod from_meta_info;
mod types;

// re-exports
pub use types::*;

use crate::{
    bitfield::{Bitfield, VczBitfield},
    config::ResolvedConfig,
    counter::Counter,
    daemon::{DaemonCtx, DaemonMsg},
    disk::{DiskMsg, ReturnToDisk},
    error::Error,
    extensions::{BLOCK_LEN, BlockInfo},
    metainfo::MetaInfo,
    peer::{self, Peer, PeerCtx, PeerId, PeerMsg},
    torrent,
    tracker::{Tracker, TrackerMsg, TrackerTrait, event::Event},
    utils::to_human_readable,
};
use hashbrown::{HashMap, HashSet};
use rand::Rng;
use std::{
    collections::BTreeMap,
    net::{IpAddr, SocketAddr},
    sync::{Arc, atomic::Ordering},
    time::Duration,
};
use tokio::{
    net::TcpStream,
    select, spawn,
    sync::{broadcast, mpsc, oneshot},
    time::{Instant, interval, interval_at, timeout},
};
use tracing::{debug, info, trace, warn};

/// This is the main entity responsible for the high-level management of
/// a torrent download or upload.
pub struct Torrent<S: State, M: TorrentSource> {
    pub name: String,
    pub ctx: Arc<TorrentCtx>,
    pub config: Arc<ResolvedConfig>,
    pub daemon_ctx: Arc<DaemonCtx>,
    pub status: TorrentStatus,
    pub rx: mpsc::Receiver<TorrentMsg>,

    /// Bitfield representing the presence or absence of pieces for our local
    /// peer, where each bit is a piece.
    pub bitfield: Bitfield,
    pub state: S,
    pub source: M,
}

/// Context of [`Torrent`] that can be shared between other types
#[derive(Debug)]
pub struct TorrentCtx {
    pub disk_tx: mpsc::Sender<DiskMsg>,
    pub free_tx: mpsc::UnboundedSender<ReturnToDisk>,
    pub tx: mpsc::Sender<TorrentMsg>,
    pub btx: broadcast::Sender<PeerBrMsg>,
    pub info_hash: InfoHash,
}

impl<M: TorrentSource> Torrent<Idle, M> {
    /// Start the torrent by first connecting to all trackers concurrently and
    /// then returns a connected torrent which can be run.
    ///
    /// The function will return when it receives an announce from the first
    /// tracker, the other ones will be awaited for in the background.
    pub async fn start(self) -> Result<Torrent<Connected, M>, Error> {
        if let Some(metadata_size) = self.state.metadata_size {
            let _ = self
                .daemon_ctx
                .tx
                .send(DaemonMsg::SetMetadataSize(
                    metadata_size,
                    self.ctx.info_hash.clone(),
                ))
                .await;
        }

        let org_trackers = self.source.organize_trackers();
        let udp_trackers = org_trackers.get("udp").unwrap().clone();
        let udp_trackers_len = udp_trackers.len();
        drop(org_trackers);

        if udp_trackers.is_empty() {
            return Err(Error::NoUDPTracker);
        }

        info!("connecting to {} udp trackers", udp_trackers.len());

        let (tracker_tx, _tracker_rx) = broadcast::channel::<TrackerMsg>(10);
        let mut tracker_tasks = Vec::with_capacity(udp_trackers.len());

        let info_hash = self.ctx.info_hash.clone();
        let local_peer_id = self.daemon_ctx.local_peer_id.clone();
        let torrent_tx = self.ctx.tx.clone();
        let size = self.source.size();
        let downloaded = self.bitfield.count_ones() as u64 * BLOCK_LEN as u64;

        let (first_response_tx, mut first_response_rx) = mpsc::channel(1);

        for tracker in udp_trackers {
            let info_hash_clone = info_hash.clone();
            let local_peer_id_clone = local_peer_id.clone();
            let torrent_tx_clone = torrent_tx.clone();
            let tracker_tx_clone = tracker_tx.clone();
            let config = self.config.clone();
            let first_response_tx = first_response_tx.clone();

            tracker_tasks.push(tokio::spawn(async move {
                match Self::announce(
                    &tracker,
                    info_hash_clone,
                    local_peer_id_clone,
                    torrent_tx_clone.clone(),
                    tracker_tx_clone.subscribe(),
                    size,
                    config,
                    downloaded,
                )
                .await
                {
                    Ok((st, peers)) => {
                        info!("connected to a tracker");
                        if first_response_tx
                            .try_send((peers.clone(), st))
                            .is_ok()
                        {
                        } else {
                            // oneshot is closed, only send to torrent task
                            let _ = torrent_tx_clone
                                .send(TorrentMsg::AddIdlePeers(
                                    peers.clone().into_iter().collect(),
                                ))
                                .await;
                        }

                        Ok(())
                    }
                    Err(e) => Err(e),
                }
            }));
        }

        let (idle_peers, stats) = match tokio::time::timeout(
            Duration::from_secs(5),
            first_response_rx.recv(),
        )
        .await
        {
            Ok(Some(r)) => (r.0.into_iter().collect(), r.1),
            _ => (
                // * 50 because a tracker typically sends 50 peers.
                HashSet::with_capacity(udp_trackers_len * 50),
                Stats::default(),
            ),
        };

        tokio::spawn(async move {
            futures::future::join_all(tracker_tasks).await;
        });

        // try to reconnect with errored peers
        let reconnect_interval = interval(Duration::from_secs(5));

        // send state to the daemon
        let heartbeat_interval = interval(Duration::from_secs(1));

        let log_rates_interval = interval(Duration::from_secs(5));

        // unchoke the slowest interested peer and unchoke a random one.
        let optimistic_unchoke_interval = interval(Duration::from_secs(30));

        let now = Instant::now();

        // unchoke algorithm:
        // - choose the best 3 interested uploaders and unchoke them.
        let unchoke_interval =
            interval_at(now + Duration::from_secs(10), Duration::from_secs(10));

        Ok(Torrent {
            config: self.config.clone(),
            state: Connected {
                tracker_tx,
                reconnect_interval,
                heartbeat_interval,
                log_rates_interval,
                optimistic_unchoke_interval,
                unchoke_interval,
                peer_pieces: HashMap::default(),
                counter: Counter::default(),
                size,
                unchoked_peers: Vec::with_capacity(3),
                opt_unchoked_peer: None,
                error_peers: Vec::with_capacity(
                    self.config.max_torrent_peers as usize,
                ),
                stats,
                idle_peers: idle_peers.into_iter().collect(),
                metadata_size: self.state.metadata_size,
                connected_peers: Vec::with_capacity(
                    self.config.max_torrent_peers as usize,
                ),
                info_pieces: BTreeMap::new(),
            },
            source: self.source,
            bitfield: self.bitfield,
            ctx: self.ctx,
            daemon_ctx: self.daemon_ctx,
            name: self.name,
            rx: self.rx,
            status: self.status,
        })
    }
}

impl<M: TorrentSource, S: torrent::State> Torrent<S, M> {
    /// helper function to connect and announce to a tracker, that can be used
    /// in tasks.
    async fn announce(
        tracker: &str,
        info_hash: InfoHash,
        local_peer_id: PeerId,
        tx: mpsc::Sender<TorrentMsg>,
        tracker_rx: broadcast::Receiver<TrackerMsg>,
        size: u64,
        config: Arc<ResolvedConfig>,
        downloaded: u64,
    ) -> Result<(Stats, Vec<SocketAddr>), Error> {
        let mut tracker = Tracker::connect_to_tracker(
            tracker,
            info_hash,
            local_peer_id,
            tracker_rx,
            tx,
            config,
        )
        .await?;

        let left = size.saturating_sub(downloaded);

        // calc downloaded uploaded and left
        let res = tracker.announce(Event::Started, downloaded, 0, left).await?;

        spawn(async move {
            tracker.run().await?;
            Ok::<(), Error>(())
        });
        Ok(res)
    }
}

impl<M: TorrentSource> Torrent<Connected, M> {
    async fn reconnect_peers(&mut self) {
        trace!(
            "reconnect_interval connected_peers: {} error_peers: {}",
            self.state.connected_peers.len(),
            self.state.error_peers.len(),
        );
        let _ = self.reconnect_errored_peers().await;
    }

    async fn unchoke(&mut self) {
        let best = self.get_best_interested_downloaders();
        println!("connected_peers of {:?}", self.daemon_ctx.local_peer_id);
        for p in &self.state.connected_peers {
            println!(
                "l {:?} r {:?} id {:?}",
                p.local_addr.port(),
                p.remote_addr.port(),
                p.id
            );
        }
        println!("------");

        // choke peers no longer in top 3
        for peer in &self.state.unchoked_peers {
            if !best.iter().any(|p| p.id == peer.id) {
                let _ = peer.tx.send(PeerMsg::Choke).await;
            }
        }

        self.state.unchoked_peers = best;

        for peer in &self.state.unchoked_peers {
            if peer.am_choking.load(Ordering::Acquire) {
                let _ = peer.tx.send(PeerMsg::Unchoke).await;
            }
        }
    }

    async fn optimistic_unchoke(&mut self) {
        if let Some(old_opt) = self.state.opt_unchoked_peer.take() {
            // only choke if not in top 3
            if !self.state.unchoked_peers.iter().any(|p| p.id == old_opt.id) {
                let _ = old_opt.tx.send(PeerMsg::Choke).await;
            }
        }

        // select new optimistic unchoke
        if let Some(new_opt) = self.get_random_choked_interested_peer() {
            debug!("optimistically unchoking {:?}", new_opt.id);
            let _ = new_opt.tx.send(PeerMsg::Unchoke).await;
            self.state.opt_unchoked_peer = Some(new_opt);
        }
    }

    fn log_rates_interval(&self) {
        let downloaded =
            self.state.counter.total_download().min(self.state.size);
        let uploaded = self.state.counter.total_upload();
        let download_rate = self.state.counter.download_rate();
        let upload_rate = self.state.counter.upload_rate();

        debug!(
            "idle {} connected {} error {}",
            self.state.idle_peers.len(),
            self.state.connected_peers.len(),
            self.state.error_peers.len(),
        );

        debug!(
            "d: {} u: {} dr: {} ur: {} p: {} dp: {}",
            to_human_readable(downloaded as f64),
            to_human_readable(uploaded as f64),
            download_rate,
            upload_rate,
            self.bitfield.len(),
            self.bitfield.count_ones()
        );
    }

    async fn heartbeat_interval(&mut self) {
        for peer in &self.state.connected_peers {
            let uploaded = peer.counter.window_uploaded_u64();
            let downloaded = peer.counter.window_downloaded_u64();

            peer.counter.update_rates();

            self.state.counter.record_upload(uploaded);
            self.state.counter.record_download(downloaded);
        }
        self.state.counter.update_rates();

        let torrent_state: TorrentState = (&*self).into();

        let _ = self
            .daemon_ctx
            .tx
            .send(DaemonMsg::TorrentState(torrent_state))
            .await;
    }

    fn read_peer_by_ip(
        &self,
        ip: IpAddr,
        port: u16,
        otx: oneshot::Sender<Option<Arc<PeerCtx>>>,
    ) {
        if let Some(peer_ctx) =
            self.state.connected_peers.iter().find(|&p| {
                p.remote_addr.ip() == ip && port == p.remote_addr.port()
            })
        {
            let _ = otx.send(Some(peer_ctx.clone()));
        } else {
            let _ = otx.send(None);
        }
    }

    async fn metadata_size(&mut self, metadata_size: usize) {
        if self.state.metadata_size.is_some() {
            return;
        };
        self.state.metadata_size = Some(metadata_size);

        let info_hash = self.ctx.info_hash.clone();

        let _ = self
            .daemon_ctx
            .tx
            .send(DaemonMsg::SetMetadataSize(
                metadata_size,
                self.ctx.info_hash.clone(),
            ))
            .await;

        let _ = self
            .ctx
            .disk_tx
            .send(DiskMsg::MetadataSize(info_hash, metadata_size))
            .await;
    }

    async fn downloaded_piece(&mut self, piece: usize) {
        debug!("downloaded_piece {piece}");

        self.bitfield.safe_set(piece);

        let _ = self.ctx.btx.send(PeerBrMsg::HavePiece(piece));

        let total_pieces = self.bitfield.len();
        let downloaded_pieces = self.bitfield.count_ones();

        debug!("downloaded pieces {}", downloaded_pieces);

        let is_download_complete = downloaded_pieces >= total_pieces;

        if !is_download_complete && self.status != TorrentStatus::Seeding {
            return;
        }

        info!("downloaded entire torrent, entering seed only mode.");
        self.status = TorrentStatus::Seeding;

        let _ = self.state.tracker_tx.send(TrackerMsg::Announce {
            event: Event::Completed,
            downloaded: self.state.counter.total_download(),
            uploaded: self.state.counter.total_upload(),
            left: 0,
        });

        let _ = self
            .ctx
            .disk_tx
            .send(DiskMsg::FinishedDownload(self.source.info_hash()))
            .await;

        let _ = self.ctx.btx.send(PeerBrMsg::Seedonly);
    }

    async fn peer_error(&mut self, addr: SocketAddr) {
        self.state.error_peers.push(Peer::<peer::PeerError>::new(addr));
        self.state.connected_peers.retain(|v| v.remote_addr != addr);
        self.state.unchoked_peers.retain(|v| v.remote_addr != addr);
        // self.state.connecting_peers.retain(|v| *v != addr);
        self.state.idle_peers.remove(&addr);

        if let Some(opt_addr) =
            self.state.opt_unchoked_peer.as_ref().map(|v| v.remote_addr)
            && opt_addr == addr
        {
            self.state.opt_unchoked_peer = None;
        }

        let _ = self.ctx.disk_tx.send(DiskMsg::DeletePeer(addr)).await;
        let _ =
            self.daemon_ctx.tx.send(DaemonMsg::DecrementConnectedPeers).await;
    }

    async fn peer_connected(&mut self, ctx: Arc<PeerCtx>) {
        self.state.connected_peers.push(ctx.clone());
        // self.state.connecting_peers.retain(|v| *v != ctx.remote_addr);
        self.state.error_peers.retain(|v| v.state.addr != ctx.remote_addr);
        self.state.idle_peers.remove(&ctx.remote_addr);

        let _ =
            ctx.torrent_ctx.disk_tx.send(DiskMsg::NewPeer(ctx.clone())).await;
        let _ = ctx.torrent_ctx.btx.send(PeerBrMsg::NewPeer(ctx.clone()));
        let _ =
            self.daemon_ctx.tx.send(DaemonMsg::IncrementConnectedPeers).await;
    }

    fn toggle_pause(&mut self) {
        if self.status != TorrentStatus::Downloading
            || self.status != TorrentStatus::Seeding
            || self.status != TorrentStatus::Paused
        {
            return;
        }

        info!("toggle pause");

        if self.status == TorrentStatus::Paused {
            let _ = self.ctx.btx.send(PeerBrMsg::Resume);
        } else {
            let _ = self.ctx.btx.send(PeerBrMsg::Pause);
        }

        if self.status == TorrentStatus::Paused {
            self.status =
                if self.state.counter.total_download() >= self.state.size {
                    TorrentStatus::Seeding
                } else {
                    TorrentStatus::Downloading
                };
        } else {
            self.status = TorrentStatus::Paused;
        };
    }

    fn quit(&mut self) {
        let tracker_tx = &self.state.tracker_tx;
        let _ = self.ctx.btx.send(PeerBrMsg::Quit);
        let downloaded = self.state.counter.total_download();

        let _ = tracker_tx.send(TrackerMsg::Announce {
            event: Event::Stopped,
            downloaded,
            uploaded: self.state.counter.total_upload(),
            left: self.state.size.saturating_sub(downloaded),
        });
    }

    /// Return a number of available connections that the torrent can do.
    async fn available_connections(&self) -> Result<usize, Error> {
        let (otx, orx) = oneshot::channel();
        self.daemon_ctx.tx.send(DaemonMsg::GetConnectedPeers(otx)).await?;
        let daemon_connected_peers = orx.await?;
        let max_global_peers = self.config.max_global_peers;
        let max_torrent_peers = self.config.max_torrent_peers;
        let currently_active = self.state.connected_peers.len();

        if currently_active >= max_torrent_peers as usize
            || daemon_connected_peers >= max_global_peers
        {
            return Ok(0);
        }

        Ok(max_torrent_peers as usize - currently_active)
    }

    // todo: implement reconnect algo
    pub async fn reconnect_errored_peers(&mut self) -> Result<(), Error> {
        // let errored: Vec<_> =
        //     self.state.error_peers.drain(..).map(|v| v.state.addr).collect();
        // self.state.idle_peers.extend(errored);

        Ok(())
    }

    /// Get the slowest peers, clone and send `qnt` block infos to the given
    /// `peer_tx`.
    pub async fn clone_block_infos_to_peer(
        &mut self,
        mut qnt: usize,
        peer_tx: mpsc::Sender<PeerMsg>,
    ) -> Result<(), Error> {
        // sort connected peers by slowest to fastest.
        self.state.connected_peers.sort_by(|a, b| {
            a.counter.download_rate_u64().cmp(&b.counter.download_rate_u64())
        });

        let mut r = Vec::with_capacity(qnt);

        for p in &self.state.connected_peers {
            let (otx, orx) = oneshot::channel();
            p.tx.send(PeerMsg::CloneBlocks(qnt, otx)).await?;
            r.extend(orx.await?);

            qnt = qnt.saturating_sub(r.len());

            if qnt == 0 {
                let mut tree: BTreeMap<usize, Vec<BlockInfo>> = BTreeMap::new();

                for info in r {
                    let e = tree.entry(info.index).or_default();
                    e.push(info);
                }

                if peer_tx.send(PeerMsg::Blocks(tree.clone())).await.is_err() {
                    let _ = self.ctx.free_tx.send(ReturnToDisk::Block(
                        self.ctx.info_hash.clone(),
                        tree,
                    ));
                }

                break;
            }
        }

        Ok(())
    }

    /// Try to connect to all idle peers (if there is capacity) and run their
    /// event loops.
    pub async fn spawn_outbound_peers(
        &self,
        have_info: bool,
    ) -> Result<(), Error> {
        let ctx = self.ctx.clone();
        let daemon_ctx = self.daemon_ctx.clone();
        let to_request = self.available_connections().await?;
        let metadata_size = self.state.metadata_size;
        let is_seed_only = self.status == TorrentStatus::Seeding;
        println!("peer list: {:#?}", self.state.idle_peers);

        for peer in self.state.idle_peers.iter().take(to_request).cloned() {
            let ctx = ctx.clone();
            let daemon_ctx = daemon_ctx.clone();
            let torrent_ctx = self.ctx.clone();

            // send connections to other peers
            spawn(async move {
                match timeout(Duration::from_secs(5), TcpStream::connect(peer))
                    .await
                {
                    Ok(Ok(socket)) => {
                        let idle_peer = Peer::<peer::Idle>::new();
                        // println!(
                        //     "outbound l: {} r: {}",
                        //     socket.local_addr().unwrap().port(),
                        //     socket.peer_addr().unwrap().port(),
                        // );

                        let Ok(mut connected_peer) = idle_peer
                            .outbound_handshake(
                                socket,
                                daemon_ctx,
                                torrent_ctx,
                                metadata_size,
                            )
                            .await
                        else {
                            ctx.tx.send(TorrentMsg::PeerError(peer)).await?;
                            return Err(Error::HandshakeInvalid);
                        };

                        connected_peer.state.have_info = have_info;
                        connected_peer.state.seed_only = is_seed_only;

                        if let Err(r) = connected_peer.run().await {
                            debug!(
                                "{} peer loop stopped due to an error: {r:?}",
                                connected_peer.state.ctx.remote_addr
                            );
                            ctx.tx.send(TorrentMsg::PeerError(peer)).await?;
                            connected_peer.free_pending_blocks();
                            return Err(r);
                        }
                    }
                    Ok(Err(_e)) => {
                        ctx.tx.send(TorrentMsg::PeerError(peer)).await?;
                    }
                    // timeout
                    Err(_) => {
                        ctx.tx.send(TorrentMsg::PeerError(peer)).await?;
                    }
                }

                Ok::<(), Error>(())
            });
        }

        Ok(())
    }

    /// Get the best n downloaders that are interested in the client.
    pub fn get_best_interested_downloaders(&self) -> Vec<Arc<PeerCtx>> {
        self.sort_peers_by_rate(true, false)
    }

    pub fn get_next_opt_unchoked_peer(&self) -> Option<Arc<PeerCtx>> {
        let mut min = u64::MAX;
        let mut result = None;

        for peer in &self.state.connected_peers {
            let peer_uploaded = peer.counter.upload_rate_u64();

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
        skip_uninterested: bool,
        is_asc: bool,
    ) -> Vec<Arc<PeerCtx>> {
        let peers = &self.state.connected_peers;

        if peers.is_empty() {
            return Vec::new();
        }

        let n = peers.len().min(3);
        let mut buffer = [(u64::MIN, usize::MIN); 3];
        let mut len = 0;

        for (index, peer) in peers.iter().enumerate() {
            if skip_uninterested
                && !peer.peer_interested.load(Ordering::Acquire)
            {
                continue;
            }

            let download_rate = peer.counter.download_rate_u64();

            if len < n {
                // insert new element
                buffer[len] = (download_rate, index);
                let mut pos = len;

                // bubble up to maintain order
                while pos > 0
                    && if is_asc {
                        buffer[pos].0 < buffer[pos - 1].0
                    } else {
                        buffer[pos].0 > buffer[pos - 1].0
                    }
                {
                    buffer.swap(pos, pos - 1);
                    pos -= 1;
                }
                len += 1;
            } else if if is_asc {
                download_rate < buffer[n - 1].0
            } else {
                download_rate > buffer[n - 1].0
            } {
                // replace smallest element in top list
                buffer[n - 1] = (download_rate, index);
                let mut pos = n - 1;

                // bubble up to maintain descending order
                while pos > 0
                    && if is_asc {
                        buffer[pos].0 < buffer[pos - 1].0
                    } else {
                        buffer[pos].0 > buffer[pos - 1].0
                    }
                {
                    buffer.swap(pos, pos - 1);
                    pos -= 1;
                }
            }
        }

        buffer[..n].iter().map(|&(_, idx)| peers[idx].clone()).collect()
    }

    fn get_random_choked_interested_peer(&self) -> Option<Arc<PeerCtx>> {
        let mut rng = rand::rng();
        let mut candidates = Vec::new();

        for peer in &self.state.connected_peers {
            // skip already unchoked peers (regular or optimistic)
            if self.state.unchoked_peers.iter().any(|p| p.id == peer.id)
                || self.state.opt_unchoked_peer.as_ref().map(|p| &p.id)
                    == Some(&peer.id)
            {
                continue;
            }

            // only consider interested peers
            if peer.peer_interested.load(Ordering::Acquire) {
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

    /// Return the first piece that the remote peer has and the local client
    /// hasn't.
    pub fn peer_has_piece_not_in_local(
        &self,
        peer_id: &PeerId,
    ) -> Option<usize> {
        let local = &self.bitfield;

        // if local has no pieces, return the first piece that remote has
        if local.count_ones() == 0 {
            return Some(0);
        };

        let remote = self.state.peer_pieces.get(peer_id)?;

        remote
            .iter_ones()
            .find(|&piece_index| !unsafe { *local.get_unchecked(piece_index) })
    }

    /// Return a bitfield representing the pieces that the local client does not
    /// have, and that the remote has.
    pub fn get_missing_pieces(&self, peer_id: &PeerId) -> Bitfield {
        self.state
            .peer_pieces
            .get(peer_id)
            // even though i'm doing a clone here, the compiler *probably*
            // optimizes this with SIMD.
            .map(|remote| !self.bitfield.clone() & remote)
            .unwrap_or_default()
    }
}
