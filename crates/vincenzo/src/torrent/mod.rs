//! Torrent that is spawned by the Daemonpeer_choking
//!
//! A torrent will manage multiple peers, peers can send messages to the torrent
//! using [`TorrentMsg`], and torrent can send messages to the Peers using
//! [`PeerMsg`].

mod from_magnet;
mod from_meta_info;
mod types;

use hashbrown::HashMap;
use rand::Rng;
// re-exports
pub use types::*;

use crate::{
    bitfield::{Bitfield, VczBitfield},
    config::CONFIG,
    counter::Counter,
    daemon::{DaemonCtx, DaemonMsg},
    disk::{DiskMsg, ReturnToDisk},
    error::Error,
    metainfo::MetaInfo,
    peer::{self, Peer, PeerCtx, PeerId, PeerMsg},
    tracker::{Tracker, TrackerMsg, TrackerTrait, event::Event},
    utils::to_human_readable,
};
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
    pub daemon_ctx: Arc<DaemonCtx>,
    pub status: TorrentStatus,
    pub rx: mpsc::Receiver<TorrentMsg>,

    /// Bitfield representing the presence or absence of pieces for our local
    /// peer, where each bit is a piece.
    pub bitfield: Bitfield,
    pub(crate) state: S,
    pub(crate) source: M,
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
    /// Start the Torrent, by sending `connect` and `announce_exchange`
    /// messages to one of the trackers, and returning a list of peers.
    pub async fn start(self) -> Result<Torrent<Connected, M>, Error> {
        let org_trackers = self.source.organize_trackers();

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

        // todo: support multi tracker torrents.
        // after connecting to the first tracker, have an interval that
        // calculates the number of missing active peers from the local
        // torrent limit and request more to other trackers.
        //
        // create a cancellation token for all trackers,
        // store idle trackers and active trackers.

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

        debug!(
            "connected to tracker: {:?}, sending announce",
            tracker.ctx.tracker_addr
        );

        let (res, payload) =
            tracker.announce(Event::Started, 0, 0, self.source.size()).await?;

        debug!("{res:?}");

        let peers = tracker.parse_compact_peer_list(payload.as_ref())?;

        let stats = Stats {
            interval: res.interval,
            seeders: res.seeders,
            leechers: res.leechers,
        };

        debug!("{stats:?}");

        let tracker_ctx = Arc::new(tracker.ctx.clone());

        spawn(async move {
            tracker.run().await?;
            Ok::<(), Error>(())
        });

        drop(org_trackers);

        // try to reconnect with errored peers
        let reconnect_interval = interval(Duration::from_secs(5));

        // send state to the frontend, if connected.
        let heartbeat_interval = interval(Duration::from_secs(1));

        let log_rates_interval = interval(Duration::from_secs(5));

        // unchoke the slowest interested peer.
        let optimistic_unchoke_interval = interval(Duration::from_secs(30));

        let now = Instant::now();

        // unchoke algorithm:
        // - choose the best 3 interested uploaders and unchoke them.
        let unchoke_interval =
            interval_at(now + Duration::from_secs(10), Duration::from_secs(10));

        let announce_interval = interval_at(
            now + Duration::from_secs(stats.interval.into()),
            Duration::from_secs(stats.interval.into()),
        );

        Ok(Torrent {
            state: Connected {
                reconnect_interval,
                heartbeat_interval,
                log_rates_interval,
                optimistic_unchoke_interval,
                unchoke_interval,
                announce_interval,
                peer_pieces: HashMap::default(),
                counter: Counter::default(),
                size: self.source.size(),
                unchoked_peers: Vec::with_capacity(3),
                opt_unchoked_peer: None,
                connecting_peers: Vec::with_capacity(
                    CONFIG.max_torrent_peers as usize,
                ),
                error_peers: Vec::with_capacity(
                    CONFIG.max_torrent_peers as usize,
                ),
                stats,
                idle_peers: peers,
                tracker_ctx,
                metadata_size: self.state.metadata_size,
                connected_peers: Vec::with_capacity(
                    CONFIG.max_torrent_peers as usize,
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

impl<M: TorrentSource> Torrent<Connected, M> {
    async fn reconnect_interval(&mut self) {
        trace!(
            "reconnect_interval connected_peers: {} error_peers: {}",
            self.state.connected_peers.len(),
            self.state.error_peers.len(),
        );
        let _ = self.reconnect_errored_peers().await;
    }

    async fn announce_interval(&mut self) -> Result<Instant, Error> {
        let (otx, orx) = oneshot::channel();
        let tracker_tx = &self.state.tracker_ctx.tx;

        let downloaded = self.state.counter.total_download();

        let _ = tracker_tx
            .send(TrackerMsg::Announce {
                event: Event::None,
                recipient: Some(otx),
                downloaded,
                uploaded: self.state.counter.total_upload(),
                left: self.state.size.saturating_sub(downloaded),
            })
            .await;

        let (resp, _payload) = orx.await?;
        trace!("new stats {resp:#?}");

        // update our stats, received from the tracker
        self.state.stats = resp.into();

        Ok(Instant::now()
            + Duration::from_secs(self.state.stats.interval as u64))
    }

    async fn unchoke_interval(&mut self) {
        trace!("unchoke_interval");
        let best = self.get_best_interested_downloaders(3);

        // choke peers no longer in top 3
        for peer in &self.state.unchoked_peers {
            if !best.iter().any(|p| p.id == peer.id) {
                trace!("choking peer {:?}", peer.id);
                let _ = peer.tx.send(PeerMsg::Choke).await;
            }
        }

        for uploader in &best {
            if !self.state.unchoked_peers.iter().any(|p| p.id == uploader.id) {
                trace!("unchoking peer {:?}", uploader.id);

                let _ = uploader.tx.send(PeerMsg::Unchoke).await;
                self.state.unchoked_peers.push(uploader.clone());
            }
        }
    }

    async fn optimistic_unchoke_interval(&mut self) {
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
        let is_download_complete = downloaded_pieces >= total_pieces;

        if !is_download_complete && self.status == TorrentStatus::Downloading {
            return;
        }

        info!("downloaded entire torrent, entering seed only mode.");
        self.status = TorrentStatus::Seeding;

        let (otx, orx) = oneshot::channel();

        let _ = self
            .state
            .tracker_ctx
            .tx
            .send(TrackerMsg::Announce {
                event: Event::Completed,
                recipient: Some(otx),
                downloaded: self.state.counter.total_download(),
                uploaded: self.state.counter.total_upload(),
                left: 0,
            })
            .await;

        let _ = self
            .ctx
            .disk_tx
            .send(DiskMsg::FinishedDownload(self.source.info_hash()))
            .await;

        if let Ok(r) = orx.await {
            debug!("announced completion with success {r:?}");
            self.state.stats = r.0.into();
        }

        let _ = self.ctx.btx.send(PeerBrMsg::Seedonly);
    }

    async fn peer_error(&mut self, addr: SocketAddr) {
        self.state.error_peers.push(Peer::<peer::PeerError>::new(addr));
        self.state.connected_peers.retain(|v| v.remote_addr != addr);
        self.state.unchoked_peers.retain(|v| v.remote_addr != addr);

        if let Some(opt_addr) =
            self.state.opt_unchoked_peer.as_ref().map(|v| v.remote_addr)
            && opt_addr == addr
        {
            self.state.opt_unchoked_peer = None;
        }
        self.state.idle_peers.retain(|v| *v != addr);

        let _ = self.ctx.disk_tx.send(DiskMsg::DeletePeer(addr)).await;

        let _ =
            self.daemon_ctx.tx.send(DaemonMsg::DecrementConnectedPeers).await;
    }

    async fn peer_connected(&mut self, ctx: Arc<PeerCtx>) {
        trace!("peer_connected");

        self.state.connected_peers.push(ctx.clone());
        self.state.connecting_peers.retain(|v| *v != ctx.remote_addr);

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

    async fn quit(&mut self) {
        info!("quitting torrent {:?}", self.name);
        let tracker_tx = &self.state.tracker_ctx.tx;

        let _ = self.ctx.btx.send(PeerBrMsg::Quit);

        let downloaded = self.state.counter.total_download();

        let _ = tracker_tx
            .send(TrackerMsg::Announce {
                event: Event::Stopped,
                recipient: None,
                downloaded,
                uploaded: self.state.counter.total_upload(),
                left: self.state.size.saturating_sub(downloaded),
            })
            .await;
    }

    /// Return a number of available connections that the torrent can do.
    async fn available_connections(&self) -> Result<usize, Error> {
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

    /// Spawn an event loop for each peer
    pub async fn spawn_outbound_peers(
        &self,
        have_info: bool,
    ) -> Result<(), Error> {
        let ctx = self.ctx.clone();
        let daemon_ctx = self.daemon_ctx.clone();

        let to_request = self.available_connections().await?;

        trace!(
            "{:?} sending handshakes to {to_request} peers",
            self.ctx.info_hash
        );

        let metadata_size = self.state.metadata_size;

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

                        let mut connected_peer = idle_peer
                            .outbound_handshake(
                                socket,
                                daemon_ctx,
                                torrent_ctx,
                                metadata_size,
                            )
                            .await?;

                        connected_peer.state.have_info = have_info;

                        if let Err(r) = connected_peer.run().await {
                            warn!(
                                "{} peer loop stopped due to an error: {r:?}",
                                connected_peer.state.ctx.remote_addr
                            );
                            connected_peer.free_pending_blocks();
                            ctx.tx.send(TorrentMsg::PeerError(peer)).await?;
                            return Err(r);
                        }
                    }
                    Ok(Err(e)) => {
                        warn!("connection fin with peer: {} {e}", peer);
                        ctx.tx.send(TorrentMsg::PeerError(peer)).await?;
                    }
                    // timeout
                    Err(_) => {}
                }

                Ok::<(), Error>(())
            });
        }

        Ok(())
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
                peer.counter.upload_rate_u64()
            } else {
                peer.counter.download_rate_u64()
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

    /// Return the first piece that the remote peer has and the local client
    /// hasn't.
    pub fn peer_has_piece_not_in_local(
        &self,
        peer_id: &PeerId,
    ) -> Option<usize> {
        let local = &self.bitfield;
        if !local.any() {
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

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
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
