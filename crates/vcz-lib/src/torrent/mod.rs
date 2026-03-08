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
    TRACKER_MSG_BOUND,
    bitfield::{Bitfield, VczBitfield},
    config::ResolvedConfig,
    counter::Counter,
    daemon::{CONNECTED_PEERS, DaemonCtx, DaemonMsg},
    disk::{DiskMsg, ReturnToDisk},
    error::Error,
    metainfo::{InfoHash, MetaInfo},
    peer::{self, Peer, PeerCtx, PeerId, PeerMsg},
    torrent,
    tracker::{Tracker, TrackerMsg, TrackerTrait, event::Event},
};
use rand::Rng;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    net::TcpStream,
    select, spawn,
    sync::{broadcast, mpsc, oneshot},
    time::{Instant, interval, interval_at, timeout},
};
use tracing::{debug, info, warn};

/// This is the main entity responsible for the high-level management of
/// a torrent download or upload.
pub(crate) struct Torrent<S: State, M: TorrentSource> {
    name: String,
    pub ctx: Arc<TorrentCtx>,
    config: Arc<ResolvedConfig>,
    daemon_ctx: Arc<DaemonCtx>,
    pub status: TorrentStatus,
    rx: mpsc::Receiver<TorrentMsg>,
    bitfield: Bitfield,
    state: S,
    pub source: M,
}

/// Context of [`Torrent`] that can be shared between other types
#[derive(Debug)]
pub struct TorrentCtx {
    pub size: AtomicU64,
    pub counter: Counter,
    pub disk_tx: mpsc::Sender<DiskMsg>,
    pub free_tx: mpsc::UnboundedSender<ReturnToDisk>,
    pub tx: mpsc::Sender<TorrentMsg>,
    pub btx: broadcast::Sender<PeerBrMsg>,
    pub info_hash: InfoHash,
    pub metadata_size: AtomicUsize,
}

impl<M: TorrentSource> Torrent<Idle, M> {
    /// Start the torrent by first connecting to all trackers concurrently and
    /// then returns a connected torrent which can be run.
    ///
    /// The function will return when it receives an announce from the first
    /// tracker, the other ones will be awaited for in the background.
    pub(crate) async fn start(self) -> Result<Torrent<Connected, M>, Error> {
        let org_trackers = self.source.organize_trackers();
        let udp_trackers = org_trackers.get("udp").unwrap().clone();
        let udp_trackers_len = udp_trackers.len();
        drop(org_trackers);

        if udp_trackers.is_empty() {
            return Err(Error::NoUDPTracker);
        }

        info!("connecting to {} udp trackers", udp_trackers.len());

        let (tracker_tx, _tracker_rx) =
            broadcast::channel::<TrackerMsg>(TRACKER_MSG_BOUND);
        let mut tracker_tasks = Vec::with_capacity(udp_trackers.len());
        let local_peer_id = self.daemon_ctx.local_peer_id.clone();
        let torrent_ctx = self.ctx.clone();
        let (first_response_tx, mut first_response_rx) = mpsc::channel(1);

        for tracker in udp_trackers {
            let local_peer_id_clone = local_peer_id.clone();
            let torrent_ctx_clone = torrent_ctx.clone();
            let tracker_tx_clone = tracker_tx.clone();
            let config = self.config.clone();
            let first_response_tx = first_response_tx.clone();

            tracker_tasks.push(tokio::spawn(async move {
                match Self::announce(
                    &tracker,
                    local_peer_id_clone,
                    torrent_ctx_clone.clone(),
                    tracker_tx_clone.subscribe(),
                    config,
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
                            let _ = torrent_ctx_clone
                                .tx
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
                stealing: Arc::new(false.into()),
                tracker_tx,
                reconnect_interval,
                heartbeat_interval,
                optimistic_unchoke_interval,
                unchoke_interval,
                peer_pieces: HashMap::default(),
                peer_pieces_diff: HashMap::default(),
                unchoked_peers: Vec::with_capacity(3),
                opt_unchoked_peer: None,
                error_peers: Vec::with_capacity(self.config.max_torrent_peers),
                stats,
                idle_peers: idle_peers.into_iter().collect(),
                connected_peers: Vec::with_capacity(
                    self.config.max_torrent_peers,
                ),
                info_pieces: BTreeMap::new(),
                peer_pieces_req: Bitfield::from_piece(self.bitfield.len()),
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
    /// Connect and announce to a tracker.
    async fn announce(
        tracker: &str,
        local_peer_id: PeerId,
        torrent_ctx: Arc<TorrentCtx>,
        tracker_rx: broadcast::Receiver<TrackerMsg>,
        config: Arc<ResolvedConfig>,
    ) -> Result<(Stats, Vec<SocketAddr>), Error> {
        let mut tracker = Tracker::connect_to_tracker(
            tracker,
            local_peer_id,
            tracker_rx,
            torrent_ctx,
            config,
        )
        .await?;

        let res = tracker.announce(Event::Started).await?;

        spawn(async move {
            tracker.run().await?;
            Ok::<(), Error>(())
        });
        Ok(res)
    }
}

impl Torrent<Connected, FromMetaInfo> {
    async fn downloaded_piece(&mut self, piece: usize) {
        debug!("downloaded_piece {piece}");

        self.bitfield.safe_set(piece, true);

        for diff in self.state.peer_pieces_diff.values_mut() {
            diff.safe_set(piece, false);
        }

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

        let _ = self
            .state
            .tracker_tx
            .send(TrackerMsg::Announce { event: Event::Completed });

        let _ = self
            .ctx
            .disk_tx
            .send(DiskMsg::FinishedDownload(
                self.source.meta_info.info.info_hash.clone(),
            ))
            .await;

        let _ = self.ctx.btx.send(PeerBrMsg::Seedonly);

        self.state.peer_pieces_req.clear();
        self.state.peer_pieces_diff.clear();
        self.state.peer_pieces.clear();
    }
}

impl<M: TorrentSource> Torrent<Connected, M> {
    async fn unchoke(&mut self) {
        let best = self.get_best_interested_downloaders();

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

    async fn heartbeat_interval(&mut self) {
        for peer in &self.state.connected_peers {
            let uploaded = peer.counter.window_uploaded_u64();
            let downloaded = peer.counter.window_downloaded_u64();

            peer.counter.update_rates();

            self.ctx.counter.record_upload(uploaded);
            self.ctx.counter.record_download(downloaded);
        }
        self.ctx.counter.update_rates();

        let mut torrent_state: TorrentState = (&*self).into();
        torrent_state.downloaded =
            torrent_state.downloaded.min(self.ctx.size.load(Ordering::Relaxed));

        let _ = self
            .daemon_ctx
            .tx
            .send(DaemonMsg::TorrentState(Box::new(torrent_state)))
            .await;
    }

    async fn peer_error(&mut self, addr: SocketAddr) {
        self.state.error_peers.push(Peer::<peer::PeerError>::new(addr));
        self.state.connected_peers.retain(|v| v.remote_addr != addr);
        self.state.unchoked_peers.retain(|v| v.remote_addr != addr);
        self.state.idle_peers.remove(&addr);

        // if the errored peer is the opt unchoked
        if let Some(opt_addr) =
            self.state.opt_unchoked_peer.as_ref().map(|v| v.remote_addr)
            && opt_addr == addr
        {
            self.state.opt_unchoked_peer = None;
        }
        CONNECTED_PEERS.fetch_sub(1, Ordering::Acquire);
    }

    async fn peer_connected(&mut self, ctx: Arc<PeerCtx>) {
        self.state.connected_peers.push(ctx.clone());
        self.state.error_peers.retain(|v| v.state.addr != ctx.remote_addr);
        self.state.idle_peers.remove(&ctx.remote_addr);
        CONNECTED_PEERS.fetch_add(1, Ordering::Acquire);
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
            self.status = if self.ctx.counter.total_download()
                >= self.ctx.size.load(Ordering::Relaxed)
            {
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
        let _ = tracker_tx.send(TrackerMsg::Announce { event: Event::Stopped });
    }

    /// Return a number of available connections that the torrent can do.
    async fn available_connections(&self) -> Result<usize, Error> {
        let daemon_connected_peers = CONNECTED_PEERS.load(Ordering::Relaxed);
        let max_global_peers = self.config.max_global_peers;
        let max_torrent_peers = self.config.max_torrent_peers;
        let currently_active = self.state.connected_peers.len();

        if currently_active >= max_torrent_peers
            || daemon_connected_peers >= max_global_peers
        {
            return Ok(0);
        }

        Ok(max_torrent_peers - currently_active)
    }

    pub(crate) async fn peer_want_blocks(
        &mut self,
        needs: usize,
        peer: Arc<PeerCtx>,
    ) -> Result<(), Error> {
        // prevent peers from calling this fn in parallel.
        if self.state.stealing.swap(true, Ordering::Acquire) {
            return Ok(());
        }

        let mut qnt = needs;
        let steal_flag = self.state.stealing.clone();

        let mut sorted_peers: Vec<_> = self
            .state
            .connected_peers
            .iter()
            .map(|p| (p.block_infos_len.load(Ordering::Relaxed), p))
            .collect();

        // sort connected peers by richest (more blocks)
        sorted_peers.sort_by_key(|b| std::cmp::Reverse(b.0));

        let thief_tx = peer.tx.clone();
        let mut r = Vec::with_capacity(qnt);

        for (_, victim) in sorted_peers {
            if victim.id == peer.id {
                continue;
            };

            if qnt == 0 {
                break;
            }

            let (otx, orx) = oneshot::channel();
            let msg = PeerMsg::Clone(qnt, otx);
            if victim.tx.send(msg).await.is_err() {
                continue;
            }

            match timeout(Duration::from_millis(1000), orx).await {
                Ok(Ok(infos)) => {
                    r.extend(infos);
                    qnt = qnt.saturating_sub(r.len());
                }
                Err(_) => {
                    warn!("deadlocked");
                }
                _ => {}
            }
        }

        let info_hash = self.ctx.info_hash.clone();
        let ftx = self.ctx.free_tx.clone();

        tokio::spawn(async move {
            if r.is_empty() {
                return;
            };
            let msg = PeerMsg::Blocks(r.clone());
            if thief_tx.send(msg).await.is_err() {
                let _ = ftx.send(ReturnToDisk::Block(info_hash, r));
            }
            steal_flag.store(false, Ordering::Release);
        });

        Ok(())
    }

    /// Try to connect to all idle peers (if there is capacity) and run their
    /// event loops.
    pub(crate) async fn spawn_outbound_peers(
        &self,
        have_info: bool,
    ) -> Result<(), Error> {
        let ctx = self.ctx.clone();
        let daemon_ctx = self.daemon_ctx.clone();
        let to_request = self.available_connections().await?;
        let is_seed_only = self.status == TorrentStatus::Seeding;

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
                        let Ok(mut connected_peer) = idle_peer
                            .outbound_handshake(socket, daemon_ctx, torrent_ctx)
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
    pub(crate) fn get_best_interested_downloaders(&self) -> Vec<Arc<PeerCtx>> {
        self.sort_peers_by_rate(true, false)
    }

    /// A fast function for returning the best or worst N amount of peers,
    /// uploaded or downloaded. Note that the maximum value of N is 3.
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

    #[inline]
    pub(crate) fn peer_have(&mut self, id: PeerId, piece: usize) {
        let bitfield = self.state.peer_pieces.entry(id.clone()).or_default();
        bitfield.safe_set(piece, true);
        let no_bitfield = !self.state.peer_pieces_diff.contains_key(&id);
        let diff = self
            .state
            .peer_pieces_diff
            .entry(id.clone())
            .or_insert(Bitfield::from_piece(piece));
        diff.safe_set(piece, !self.bitfield.safe_get(piece));
        if no_bitfield {
            let _ = self.gen_missing_pieces(id);
        }
    }

    #[inline]
    pub(crate) fn peer_has_piece_not_in_local(
        &self,
        peer_id: &PeerId,
    ) -> Option<usize> {
        if self.bitfield.count_ones() == 0 {
            return Some(0);
        }
        self.get_missing_pieces(peer_id)
            .map(|v| v.first_one().unwrap_or_default())
    }

    #[inline]
    pub(crate) fn get_missing_pieces(
        &self,
        peer_id: &PeerId,
    ) -> Option<Bitfield> {
        self.state.peer_pieces_diff.get(peer_id).cloned()
    }

    pub(crate) fn gen_missing_pieces(
        &mut self,
        peer_id: PeerId,
    ) -> Result<(), Error> {
        let remote = self
            .state
            .peer_pieces
            .get(&peer_id)
            .ok_or(Error::PeerDoesNotExist)?;
        let bitfield_len = remote.len();
        let diff = self
            .state
            .peer_pieces_diff
            .entry(peer_id)
            .or_insert_with(|| Bitfield::from_piece(bitfield_len));
        *diff = *remote.clone() & !self.bitfield.clone();
        Ok(())
    }

    pub(crate) fn get_want_pieces(
        &mut self,
        peer_id: &PeerId,
        pieces_wanted: usize,
    ) -> Result<Vec<usize>, Error> {
        let diff = self
            .state
            .peer_pieces_diff
            .get(peer_id)
            .ok_or(Error::PeerDoesNotExist)?;

        let req = &mut self.state.peer_pieces_req;
        let mut pieces = Vec::with_capacity(pieces_wanted);

        for piece in diff.iter_ones() {
            if *req.safe_get(piece) {
                continue;
            }
            req.safe_set(piece, true);
            pieces.push(piece);
            if pieces.len() == pieces_wanted {
                break;
            }
        }

        Ok(pieces)
    }
}
