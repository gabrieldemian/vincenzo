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
    daemon::DaemonMsg,
    disk::DiskMsg,
    error::Error,
    magnet::Magnet,
    metainfo::Info,
    peer::{
        session::ConnectionState, Direction, Peer, PeerBuilder, PeerCtx,
        PeerId, PeerMsg,
    },
    tracker::{event::Event, Tracker, TrackerCtx, TrackerMsg, TrackerTrait},
};
use bendy::decoding::FromBencode;
use bitvec::{bitvec, prelude::Msb0};
use hashbrown::HashMap;
use speedy::{Readable, Writable};
use std::{
    collections::BTreeMap,
    net::SocketAddr,
    sync::{atomic::AtomicBool, Arc},
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
    // Stats of the current Torrent, returned from tracker on announce
    // requests.
    pub stats: Stats,

    pub peer_ctxs: HashMap<PeerId, Arc<PeerCtx>>,
    pub have_info: bool,

    // If using a Magnet link, the info will be downloaded in pieces
    // and those pieces may come in different order,
    // hence the HashMap (dictionary), and not a vec.
    // After the dict is complete, it will be decoded into [`info`]
    pub info_pieces: BTreeMap<u32, Vec<u8>>,

    pub failed_peers: Vec<SocketAddr>,
    pub peers: Vec<SocketAddr>,

    // The downloaded bytes of the previous second,
    // used to get the download rate in seconds.
    // this will be mutated on the frontend event loop.
    pub last_second_downloaded: u64,

    pub download_rate: u64,

    // The total size of the torrent files, in bytes,
    // this is a cache of ctx.info.get_size()
    pub size: u64,
    pub tracker_ctx: Arc<TrackerCtx>,
}

impl TorrentTrait for Idle {}
impl TorrentTrait for Connected {}

/// This is the main entity responsible for the high-level management of
/// a torrent download or upload.
pub struct Torrent<S: TorrentTrait> {
    pub ctx: Arc<TorrentCtx>,
    pub daemon_tx: mpsc::Sender<DaemonMsg>,
    pub name: String,
    pub rx: mpsc::Receiver<TorrentMsg>,
    pub state: S,
    pub status: TorrentStatus,
}

/// State of a [`Torrent`], used by the UI to present data.
#[derive(Debug, Clone, Default, PartialEq, Readable, Writable)]
pub struct TorrentState {
    pub name: String,
    pub stats: Stats,
    pub status: TorrentStatus,
    pub downloaded: u64,
    pub download_rate: u64,
    pub uploaded: u64,
    pub size: u64,
    pub info_hash: InfoHash,
}

/// Context of [`Torrent`] that can be shared between other types
#[derive(Debug)]
pub struct TorrentCtx {
    pub disk_tx: mpsc::Sender<DiskMsg>,
    pub tx: mpsc::Sender<TorrentMsg>,
    pub magnet: Magnet,
    pub info_hash: InfoHash,
    pub bitfield: RwLock<Bitfield>,
    pub info: Arc<RwLock<Info>>,
    pub has_at_least_one_piece: AtomicBool,
}

/// Status of the current Torrent, updated at every announce request.
#[derive(Clone, Debug, PartialEq, Default, Readable, Writable)]
pub struct Stats {
    pub interval: u32,
    pub leechers: u32,
    pub seeders: u32,
}

impl Torrent<Idle> {
    #[tracing::instrument(skip(disk_tx, daemon_tx), name = "torrent::new")]
    pub fn new(
        disk_tx: mpsc::Sender<DiskMsg>,
        daemon_tx: mpsc::Sender<DaemonMsg>,
        magnet: Magnet,
    ) -> Torrent<Idle> {
        let name = magnet.parse_dn();
        let bitfield = RwLock::new(Bitfield::default());
        let info = Arc::new(RwLock::new(Info::default().name(name.clone())));

        let (tx, rx) = mpsc::channel::<TorrentMsg>(100);

        let ctx = Arc::new(TorrentCtx {
            tx: tx.clone(),
            disk_tx,
            info_hash: magnet.parse_xt_infohash(),
            bitfield,
            magnet,
            info,
            has_at_least_one_piece: AtomicBool::new(false),
        });

        Self {
            state: Idle,
            name,
            status: TorrentStatus::default(),
            daemon_tx,
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
            self.ctx.info.clone(),
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
                stats,
                peers,
                tracker_ctx,
                size: 0,
                peer_ctxs: HashMap::new(),
                have_info: false,
                info_pieces: BTreeMap::new(),
                failed_peers: Vec::new(),
                download_rate: 0,
                last_second_downloaded: 0,
            },
            ctx: self.ctx,
            daemon_tx: self.daemon_tx,
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
        for peer in self.state.peers.clone() {
            let ctx = self.ctx.clone();
            let local_peer_id = self.state.tracker_ctx.peer_id.clone();

            // send connections too other peers
            spawn(async move {
                match TcpStream::connect(peer).await {
                    Ok(socket) => {
                        Self::start_and_run_peer(
                            ctx,
                            socket,
                            local_peer_id,
                            Direction::Outbound,
                        )
                        .await?;
                    }
                    Err(e) => {
                        debug!("error with peer: {:?} {e:#?}", peer);
                        ctx.tx.send(TorrentMsg::FailedPeer(peer)).await?;
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
                let _local_peer_id = local_peer_id.clone();

                if let Ok((socket, addr)) = local_peer_socket.accept().await {
                    info!("received inbound connection from {addr}");
                    let ctx = ctx.clone();

                    spawn(async move {
                        Self::start_and_run_peer(
                            ctx,
                            socket,
                            _local_peer_id,
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
    ) -> Result<Peer, Error> {
        let handshaked_peer = PeerBuilder::default()
            .socket(socket)
            .direction(direction)
            .local_peer_id(local_peer_id)
            .torrent_ctx(ctx.clone())
            .handshake()
            .await?;

        let mut peer = Peer::from(handshaked_peer);

        let r = peer.run().await;

        if let Err(r) = r {
            debug!(
                "{} Peer session stopped due to an error: {}",
                peer.ctx.local_addr, r
            );
        }
        // if we are gracefully shutting down, we do nothing with the pending
        // blocks, Rust will drop them when their scope ends naturally.
        // otherwise, we send the blocks back to the torrent
        // so that other peers can download them. In this case, the peer
        // might be shutting down due to an error or this is malicious peer
        // that we wish to end the connection.
        if peer.session.state.connection != ConnectionState::Quitting {
            peer.free_pending_blocks().await;
        }

        Ok(peer)
    }

    /// Run the Torrent main event loop to listen to internal [`TorrentMsg`].
    #[tracing::instrument(skip_all)]
    pub async fn run(&mut self) -> Result<(), Error> {
        debug!("running torrent: {:?}", self.name);

        let tracker_tx = self.state.tracker_ctx.tx.clone();

        let mut announce_interval = interval_at(
            Instant::now()
                + Duration::from_secs(
                    self.state.stats.interval.max(500).into(),
                ),
            Duration::from_secs((self.state.stats.interval as u64).max(500)),
        );

        let mut reconnect_failed_peers = interval_at(
            Instant::now() + Duration::from_secs(5),
            Duration::from_secs(5),
        );

        let mut frontend_interval = interval(Duration::from_secs(1));

        loop {
            select! {
                Some(msg) = self.rx.recv() => {
                    match msg {
                        TorrentMsg::DownloadedPiece(piece) => {
                            self.ctx.has_at_least_one_piece.store(
                                true,
                                std::sync::atomic::Ordering::Relaxed
                            );
                            // send Have messages to peers that dont have our pieces
                            for peer in self.state.peer_ctxs.values() {
                                let _ = peer.tx.send(PeerMsg::HavePiece(piece)).await;
                            }
                        }
                        TorrentMsg::PeerConnected(id, ctx) => {
                            debug!("{} connected with {}", ctx.local_addr, ctx.remote_addr);

                            self.state.peer_ctxs.insert(id, ctx.clone());

                            let _ = self
                                .ctx
                                .disk_tx
                                .send(DiskMsg::NewPeer(ctx))
                                .await;
                        }
                        TorrentMsg::DownloadComplete => {
                            info!("Downloaded torrent {:?}", self.name);
                            let (otx, orx) = oneshot::channel();

                            self.status = TorrentStatus::Seeding;

                            if let Some(tracker_tx) = &tracker_tx {
                                let _ = tracker_tx.send(
                                    TrackerMsg::Announce {
                                        event: Event::Completed,
                                        recipient: Some(otx),
                                    })
                                .await;
                            }

                            if let Ok(Ok(r)) = orx.await {
                                debug!("announced completion with success {r:#?}");
                                self.state.stats = r.0.into();
                            }

                            // tell all peers that we are not interested,
                            // we wont request blocks from them anymore
                            for peer in self.state.peer_ctxs.values() {
                                let _ = peer.tx.send(PeerMsg::NotInterested).await;
                                let _ = peer.tx.send(PeerMsg::SeedOnly).await;
                            }
                        }
                        // The peer "from" was the first one to receive the "info".
                        // Send Cancel messages to everyone else.
                        // todo: move to broadcast
                        TorrentMsg::SendCancelBlock { from, block_info } => {
                            for (k, peer) in self.state.peer_ctxs.iter() {
                                if *k == from { continue };
                                let _ = peer.tx.send(PeerMsg::CancelBlock(block_info.clone())).await;
                            }
                        }
                        // todo: move to broadcast
                        TorrentMsg::SendCancelMetadata { from, index } => {
                            debug!("received SendCancelMetadata {from:?} {index}");
                            for (k, peer) in self.state.peer_ctxs.iter() {
                                if *k == from { continue };
                                let _ = peer.tx.send(PeerMsg::CancelMetadata(index)).await;
                            }
                        }
                        // todo: move to broadcast
                        TorrentMsg::StartEndgame(_peer_id, block_infos) => {
                            info!("Started endgame mode for {}", self.name);
                            for (_id, peer) in self.state.peer_ctxs.iter() {
                                let _ = peer.tx.send(PeerMsg::RequestBlockInfos(block_infos.clone())).await;
                            }
                        }
                        TorrentMsg::DownloadedInfoPiece(total, index, bytes) => {
                            debug!("received DownloadedInfoPiece");

                            if self.status == TorrentStatus::ConnectingTrackers {
                                self.status = TorrentStatus::DownloadingMetainfo;
                            }

                            self.state.info_pieces.insert(index, bytes);

                            let info_len = self.state.info_pieces.values().fold(0, |acc, b| {
                                acc + b.len()
                            });

                            let have_all_pieces = info_len as u32 >= total;

                            if have_all_pieces {
                                // info has a valid bencode format
                                let info_bytes = self.state.info_pieces.values().fold(Vec::new(), |mut acc, b| {
                                    acc.extend_from_slice(b);
                                    acc
                                });
                                let info = Info::from_bencode(&info_bytes).map_err(|_| Error::BencodeError)?;

                                // todo get xt
                                let m_info = self.ctx.magnet.hash_type().unwrap();

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
                                    let mut bitfield = self.ctx.bitfield.write().await;
                                    *bitfield = bitvec![u8, Msb0; 0; info.pieces() as usize];

                                    // remove excess bits
                                    if (info.pieces() as usize) < bitfield.len() {
                                        unsafe {
                                            bitfield.set_len(info.pieces() as usize);
                                        }
                                    }

                                    debug!("local_bitfield is now of len {:?}", bitfield.len());

                                    self.state.size = info.get_size();
                                    self.state.have_info = true;

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
                            let tx = self.state.tracker_ctx.tx.as_ref().unwrap();
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
                            let tx = self.state.tracker_ctx.tx.as_ref().unwrap();
                            tx.send(TrackerMsg::Increment { downloaded: 0, uploaded }).await?;
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
                                for (_, peer) in &self.state.peer_ctxs {
                                    if self.status == TorrentStatus::Paused {
                                        let _ = peer.tx.send(PeerMsg::Pause).await;
                                    } else {
                                        let _ = peer.tx.send(PeerMsg::Resume).await;
                                    }
                                }
                            }
                        }
                        TorrentMsg::FailedPeer(addr) => {
                            self.state.failed_peers.push(addr);
                        },
                        TorrentMsg::Quit => {
                            info!("Quitting torrent {:?}", self.name);
                            let (otx, orx) = oneshot::channel();

                            if let Some(tracker_tx) = &tracker_tx {
                                let _ = tracker_tx.send(
                                    TrackerMsg::Announce {
                                        event: Event::Stopped,
                                        recipient: Some(otx),
                                    })
                                .await;
                            }

                            for peer in self.state.peer_ctxs.values() {
                                let tx = peer.tx.clone();
                                spawn(async move {
                                    let _ = tx.send(PeerMsg::Quit).await;
                                });
                            }

                            orx.await??;

                            return Ok(());
                        }
                    }
                }
                _ = frontend_interval.tick() => {
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
                    let _ = self.daemon_tx.send(DaemonMsg::TorrentState(torrent_state)).await;
                }
                // periodically announce to tracker, at the specified interval
                // to update the tracker about the client's stats.
                _ = announce_interval.tick() => {
                    let info = self.ctx.info.read().await;

                    // we know if the info is downloaded if the piece_length is > 0
                    if info.piece_length > 0 {
                        debug!("sending periodic announce, interval {announce_interval:?}");

                        let (otx, orx) = oneshot::channel();

                        if let Some(tracker_tx) = &tracker_tx {
                            let _ = tracker_tx.send(
                                TrackerMsg::Announce {
                                    event: Event::None,
                                    recipient: Some(otx),
                                })
                            .await;
                        }

                        let r = orx.await??;
                        debug!("new stats {r:#?}");

                        // update our stats, received from the tracker
                        self.state.stats = r.0.into();

                        announce_interval = interval(
                            Duration::from_secs(self.state.stats.interval as u64),
                        );
                    }
                    drop(info);
                }
                // At every 5 seconds, try to reconnect to peers in which
                // the TCP connection failed.
                _ = reconnect_failed_peers.tick() => {
                    for peer in self.state.failed_peers.clone() {
                        let ctx = self.ctx.clone();
                        let local_peer_id = self.state.tracker_ctx.peer_id.clone();
                        debug!("reconnecting_peer {peer:?}");

                        if let Ok(socket) = TcpStream::connect(peer).await {
                            self.state.failed_peers.retain(|v| *v != peer);
                            Self::start_and_run_peer(ctx, socket, local_peer_id, Direction::Outbound)
                                .await?;
                        }
                    }
                }
            }
        }
    }
}
