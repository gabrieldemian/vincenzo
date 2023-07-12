use crate::tcp_wire::lib::BlockInfo;
use crate::tcp_wire::messages::HandshakeCodec;
use crate::{
    bitfield::Bitfield,
    cli::Args,
    disk::DiskMsg,
    error::Error,
    magnet_parser::get_info_hash,
    metainfo::Info,
    peer::{Direction, Peer, PeerCtx, PeerMsg},
    tracker::{
        event::Event,
        tracker::{Tracker, TrackerCtx, TrackerMsg},
    },
};
use clap::Parser;
use core::sync::atomic::{AtomicU64, Ordering};
use magnet_url::Magnet;
use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
    time::Duration,
};
use tokio::{
    net::{TcpListener, TcpStream},
    select, spawn,
    sync::{mpsc, oneshot, RwLock},
    time::{interval, interval_at, Instant},
};
use tokio_util::codec::Framed;
use tracing::{info, warn};

#[derive(Debug)]
pub enum TorrentMsg {
    /// Message to update the torrent's Bitfield,
    /// Torrent will start with a blank bitfield
    /// because it cannot know it from a magnet link
    /// once a peer send the first bitfield message,
    /// we will update it.
    UpdateBitfield(usize),
    /// Message when one of the peers have downloaded
    /// an entire piece. We send Have messages to peers
    /// that don't have it and update the UI with stats.
    DownloadedPiece(usize),
    PeerConnected(PeerCtx),
    DownloadComplete,
}

/// This is the main entity responsible for the high-level management of
/// a torrent download or upload.
#[derive(Debug)]
pub struct Torrent {
    pub ctx: Arc<TorrentCtx>,
    pub tracker_ctx: Arc<TrackerCtx>,
    pub tx: mpsc::Sender<TorrentMsg>,
    pub disk_tx: mpsc::Sender<DiskMsg>,
    pub rx: mpsc::Receiver<TorrentMsg>,
    pub in_end_game: bool,
    pub tracker_tx: mpsc::Sender<TrackerMsg>,
    pub peer_ctxs: Vec<PeerCtx>,
}

/// Information and methods shared with peer sessions in the torrent.
/// This type contains fields that need to be read or updated by peer sessions.
/// Fields expected to be mutated are thus secured for inter-task access with
/// various synchronization primitives.
#[derive(Debug)]
pub struct TorrentCtx {
    pub magnet: Magnet,
    pub info_hash: [u8; 20],
    pub pieces: RwLock<Bitfield>,
    pub requested_blocks: RwLock<VecDeque<BlockInfo>>,
    pub downloaded_blocks: RwLock<VecDeque<BlockInfo>>,
    pub info: RwLock<Info>,
    /// If using a Magnet link, the info will be downloaded in pieces
    /// and those pieces may come in different order,
    /// hence the HashMap (dictionary), and not a vec.
    /// After the dict is complete, it will be decoded into [`info`]
    pub info_dict: RwLock<HashMap<u32, Vec<u8>>>,
    /// Stats of the current Torrent, returned from tracker on announce requests.
    pub stats: RwLock<Stats>,
    /// How many bytes we have uploaded to other peers.
    pub uploaded: AtomicU64,
    /// How many bytes we have downloaded from other peers.
    pub downloaded: AtomicU64,
}

// Status of the current Torrent, updated at every announce request.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Stats {
    pub interval: u32,
    pub leechers: u32,
    pub seeders: u32,
}

impl Torrent {
    pub async fn new(
        tx: mpsc::Sender<TorrentMsg>,
        disk_tx: mpsc::Sender<DiskMsg>,
        rx: mpsc::Receiver<TorrentMsg>,
        tracker_tx: mpsc::Sender<TrackerMsg>,
        magnet: Magnet,
    ) -> Self {
        let pieces = RwLock::new(Bitfield::default());
        let info = RwLock::new(Info::default());
        let info_dict = RwLock::new(HashMap::<u32, Vec<u8>>::new());
        let tracker_ctx = Arc::new(TrackerCtx::default());
        let requested_blocks = RwLock::new(VecDeque::new());
        let downloaded_blocks = RwLock::new(VecDeque::new());

        let xt = magnet.xt.clone().unwrap();
        let info_hash = get_info_hash(&xt);

        let ctx = Arc::new(TorrentCtx {
            stats: RwLock::new(Stats::default()),
            info_hash,
            info_dict,
            pieces,
            requested_blocks,
            magnet,
            downloaded_blocks,
            info,
            uploaded: AtomicU64::new(0),
            downloaded: AtomicU64::new(0),
        });

        Self {
            tracker_ctx,
            ctx,
            in_end_game: false,
            disk_tx,
            tx,
            tracker_tx,
            rx,
            peer_ctxs: Vec::new(),
        }
    }

    /// Start the Torrent, by sending `connect` and `announce_exchange`
    /// messages to one of the trackers, and returning a list of peers.
    #[tracing::instrument(skip(self, tracker_rx), name = "torrent::start")]
    pub async fn start(
        &mut self,
        tracker_rx: mpsc::Receiver<TrackerMsg>,
    ) -> Result<Vec<Peer>, Error> {
        let args = Args::parse();
        let mut peers: Vec<Peer> = Vec::new();
        let mut tracker = Tracker::default();

        match args.seeds {
            Some(seeds) => {
                let peers_l: Vec<Peer> = seeds
                    .into_iter()
                    .map(|addr| {
                        let (peer_tx, peer_rx) = mpsc::channel::<PeerMsg>(300);
                        let torrent_ctx = self.ctx.clone();
                        let torrent_tx = self.tx.clone();
                        let tracker_ctx = self.tracker_ctx.clone();
                        let disk_tx = self.disk_tx.clone();

                        let peer = Peer::new(peer_tx, torrent_tx, torrent_ctx, peer_rx)
                            .addr(addr)
                            .tracker_ctx(tracker_ctx)
                            .disk_tx(disk_tx);

                        peer
                    })
                    .collect();

                peers_l.into_iter().for_each(|p| peers.push(p));
            }
            None => {
                let mut tracker_l = Tracker::connect(self.ctx.magnet.tr.clone()).await?;
                let args = Args::parse();
                let info_hash = self.ctx.clone().info_hash;
                let (res, peers_l) = tracker_l.announce_exchange(info_hash, args.listen).await?;

                let mut stats = self.ctx.stats.write().await;

                *stats = Stats {
                    interval: res.interval,
                    seeders: res.seeders,
                    leechers: res.leechers,
                };
                drop(stats);

                let peers_l: Vec<Peer> = peers_l
                    .into_iter()
                    .map(|addr| {
                        let (peer_tx, peer_rx) = mpsc::channel::<PeerMsg>(300);
                        let torrent_ctx = self.ctx.clone();
                        let torrent_tx = self.tx.clone();
                        let tracker_ctx = self.tracker_ctx.clone();
                        let disk_tx = self.disk_tx.clone();

                        let peer = Peer::new(peer_tx, torrent_tx, torrent_ctx, peer_rx)
                            .addr(addr)
                            .tracker_ctx(tracker_ctx)
                            .disk_tx(disk_tx);

                        peer
                    })
                    .collect();

                tracker = tracker_l;
                peers = peers_l;

                info!("tracker.ctx peer {:?}", self.tracker_ctx.local_peer_addr);
            }
        };

        self.tracker_ctx = tracker.ctx.clone().into();

        spawn(async move {
            let _ = tracker.run(tracker_rx).await;
        });

        self.spawn_inbound_peers().await?;

        Ok(peers)
    }

    /// Spawn an event loop for each peer to listen/send messages.
    pub async fn spawn_outbound_peers(&self, peers: Vec<Peer>) -> Result<(), Error> {
        for mut peer in peers {
            info!("outbound peer {:?}", peer.addr);
            let torrent_tx = self.tx.clone();

            // send connections too other peers
            spawn(async move {
                match TcpStream::connect(peer.addr).await {
                    Ok(socket) => {
                        info!("we connected with {:?}", peer.addr);

                        let socket = Framed::new(socket, HandshakeCodec);

                        let _ = torrent_tx
                            .send(TorrentMsg::PeerConnected(peer.ctx.clone()))
                            .await;
                        let _ = peer.run(Direction::Outbound, socket).await;
                    }
                    Err(e) => {
                        warn!("error with peer: {:?} {e:#?}", peer.addr);
                    }
                }
            });
        }
        Ok(())
    }

    #[tracing::instrument(skip(self))]
    pub async fn spawn_inbound_peers(&self) -> Result<(), Error> {
        info!("running spawn inbound peers...");
        info!(
            "accepting requests in {:?}",
            self.tracker_ctx.local_peer_addr
        );

        let local_peer_socket = TcpListener::bind(self.tracker_ctx.local_peer_addr).await?;

        let disk_tx = self.disk_tx.clone();
        let torrent_ctx = Arc::clone(&self.ctx);
        let tracker_ctx = Arc::clone(&self.tracker_ctx);
        let torrent_tx = self.tx.clone();

        // accept connections from other peers
        spawn(async move {
            info!("accepting requests in {local_peer_socket:?}");

            loop {
                if let Ok((socket, addr)) = local_peer_socket.accept().await {
                    let socket = Framed::new(socket, HandshakeCodec);

                    info!("received inbound connection from {addr}");

                    let torrent_ctx = torrent_ctx.clone();
                    let (peer_tx, peer_rx) = mpsc::channel::<PeerMsg>(300);

                    let mut peer = Peer::new(peer_tx, torrent_tx.clone(), torrent_ctx, peer_rx)
                        .addr(addr)
                        .tracker_ctx(tracker_ctx.clone())
                        .disk_tx(disk_tx.clone());

                    let _ = torrent_tx
                        .send(TorrentMsg::PeerConnected(peer.ctx.clone()))
                        .await;

                    spawn(async move {
                        peer.run(Direction::Inbound, socket).await?;
                        Ok::<(), Error>(())
                    });
                }
            }
        });
        Ok(())
    }

    #[tracing::instrument(name = "torrent::run", skip(self))]
    pub async fn run(&mut self) -> Result<(), Error> {
        let mut tick_interval = interval(Duration::new(1, 0));
        let stats = self.ctx.stats.read().await;

        let mut announce_interval = interval_at(
            Instant::now() + Duration::from_secs(stats.interval.max(500).into()),
            Duration::from_secs((stats.interval as u64).max(500)),
        );
        drop(stats);

        loop {
            select! {
                Some(msg) = self.rx.recv() => {
                    // in the future, this event loop will
                    // send messages to the frontend,
                    // the terminal ui.
                    match msg {
                        TorrentMsg::UpdateBitfield(len) => {
                            // create an empty bitfield with the same
                            // len as the bitfield from the peer
                            let ctx = Arc::clone(&self.ctx);
                            let mut pieces = ctx.pieces.write().await;

                            info!("update bitfield len {:?}", len);

                            // only create the bitfield if we don't have one
                            // pieces.len() will start at 0
                            if pieces.len() < len {
                                let inner = vec![0_u8; len];
                                *pieces = Bitfield::from(inner);
                            }
                        }
                        TorrentMsg::DownloadedPiece(piece) => {
                            info!("how many peers connected {:?}", self.peer_ctxs.len());
                            info!("torrent downloadedpiece {piece}");
                            // send Have messages to peers that dont have our pieces
                            for peer in &self.peer_ctxs {
                                info!("parsing peer {:?}", peer.addr);
                                let _ = peer.peer_tx.send(PeerMsg::DownloadedPiece(piece)).await;
                            }
                        }
                        TorrentMsg::PeerConnected(ctx) => {
                            info!("connected with new peer");
                            self.peer_ctxs.push(ctx);
                            info!("len of peers {}", self.peer_ctxs.len());
                        }
                        TorrentMsg::DownloadComplete => {
                            info!("received msg download complete");
                            let (otx, orx) = oneshot::channel();
                            let downloaded = self.ctx.downloaded.load(Ordering::SeqCst);

                            let _ = self.tracker_tx.send(
                                TrackerMsg::Announce {
                                    event: Event::Completed,
                                    info_hash: self.ctx.info_hash,
                                    downloaded,
                                    uploaded: self.ctx.uploaded.load(Ordering::SeqCst),
                                    left: 0,
                                    recipient: otx,
                                })
                            .await;

                            let r = orx.await;

                            if let Ok(Ok(r)) = r {
                                info!("announced completion with success {r:#?}");
                            }

                            // announce to tracker that we are stopping
                            if Args::parse().quit_after_complete {
                                info!("exiting...");

                                let (otx, orx) = oneshot::channel();
                                let db = self.ctx.downloaded_blocks.read().await;
                                let downloaded = db.iter().fold(0, |acc, x| acc + x.len as u64);
                                let info = self.ctx.info.read().await;
                                let left = downloaded - info.get_size();
                                drop(info);

                                let _ = self.tracker_tx.send(
                                    TrackerMsg::Announce {
                                        event: Event::Stopped,
                                        info_hash: self.ctx.info_hash,
                                        downloaded,
                                        uploaded: self.ctx.uploaded.load(Ordering::SeqCst),
                                        left,
                                        recipient: otx,
                                    })
                                .await;

                                let r = orx.await;

                                if let Ok(Ok(r)) = r {
                                    info!("announced stopped with success {r:#?}");
                                }
                                std::process::exit(exitcode::OK);
                            }
                        }
                    }
                }
                // periodically announce to tracker, at the specified interval
                // to update the tracker about the client's stats.
                // let have_info = self.ctx.info_dict;
                _ = announce_interval.tick() => {
                    let info = self.ctx.info.read().await;
                    // we know if the info is downloaded if the piece_length is > 0
                    if info.piece_length > 0 {
                        info!("sending periodic announce");
                        let db = self.ctx.downloaded_blocks.read().await;
                        let downloaded = db.iter().fold(0, |acc, x| acc + x.len as u64);
                        let left = if downloaded < info.get_size() {info.get_size()} else { downloaded - info.get_size()};
                        drop(db);
                        let (otx, orx) = oneshot::channel();

                        let _ = self.tracker_tx.send(
                            TrackerMsg::Announce {
                                event: Event::None,
                                info_hash: self.ctx.info_hash,
                                downloaded,
                                uploaded: self.ctx.uploaded.load(Ordering::SeqCst),
                                left,
                                recipient: otx,
                            })
                        .await;

                        let r = orx.await;
                        info!("new stats {r:#?}");

                        // update our stats, received from the tracker
                        if let Ok(Ok(r)) = r {
                            let mut stats = self.ctx.stats.write().await;
                            stats.interval = r.interval;
                            stats.seeders = r.seeders;
                            stats.leechers = r.leechers;
                            announce_interval = interval(Duration::from_secs(r.interval as u64));
                        }
                    }
                    drop(info);
                }
                // interval used to send stats to the UI
                _ = tick_interval.tick() => {
                },
            }
        }
    }
}
