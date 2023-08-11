use crate::magnet_parser::get_magnet;
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
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::Path;
use std::{collections::VecDeque, sync::Arc, time::Duration};
use tokio::{
    net::{TcpListener, TcpStream},
    select, spawn,
    sync::{mpsc, oneshot, RwLock},
    time::{interval_at, Instant},
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
    PeerConnected(Arc<PeerCtx>),
    DownloadComplete,
}

#[derive(Debug, Default, Clone)]
pub enum TorrentStatus {
    #[default]
    Connecting,
    Downloading,
    Seeding,
    Error,
}

impl<'a> Into<&'a str> for TorrentStatus {
    fn into(self) -> &'a str {
        use TorrentStatus::*;
        match self {
            Connecting => "Connecting",
            Downloading => "Downloading",
            Seeding => "Seeding",
            Error => "Error",
        }
    }
}

impl Into<String> for TorrentStatus {
    fn into(self) -> String {
        use TorrentStatus::*;
        match self {
            Connecting => "Connecting".to_owned(),
            Downloading => "Downloading".to_owned(),
            Seeding => "Seeding".to_owned(),
            Error => "Error".to_owned(),
        }
    }
}

impl From<&str> for TorrentStatus {
    fn from(value: &str) -> Self {
        use TorrentStatus::*;
        match value {
            "Connecting" => Connecting,
            "Downloading" => Downloading,
            "Seeding" => Seeding,
            "Error" | _ => Error,
        }
    }
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
    pub peer_ctxs: Vec<Arc<PeerCtx>>,
    pub tracker_tx: Option<mpsc::Sender<TrackerMsg>>,
}

#[derive(Debug)]
pub struct TorrentCtx {
    pub tracker_tx: RwLock<Option<mpsc::Sender<TrackerMsg>>>,
    pub magnet: Magnet,
    pub info_hash: [u8; 20],
    pub pieces: RwLock<Bitfield>,
    /// The block infos of this torrent to be downloaded,
    /// when a block info is downloaded, it is moved from this Vec,
    /// to [`downloaded_blocks`].
    pub block_infos: RwLock<VecDeque<BlockInfo>>,
    /// Blocks that were requested, but not downloaded,
    /// when those are downloaded, they are NOT removed from this Vec.
    /// but copied to [`downloaded_blocks`].
    pub requested_blocks: RwLock<VecDeque<BlockInfo>>,
    /// Downloaded blocks that were previously requested.
    pub downloaded_blocks: RwLock<VecDeque<BlockInfo>>,
    pub info: RwLock<Info>,
    /// If using a Magnet link, the info will be downloaded in pieces
    /// and those pieces may come in different order,
    /// hence the HashMap (dictionary), and not a vec.
    /// After the dict is complete, it will be decoded into [`info`]
    // pub info_dict: RwLock<HashMap<u32, Vec<u8>>>,
    pub info_dict: RwLock<BTreeMap<u32, Vec<u8>>>,
    /// Stats of the current Torrent, returned from tracker on announce requests.
    pub stats: RwLock<Stats>,
    /// How many bytes we have uploaded to other peers.
    pub uploaded: Arc<AtomicU64>,
    /// How many bytes we have downloaded from other peers.
    pub downloaded: Arc<AtomicU64>,
    pub status: RwLock<TorrentStatus>,
}

// Status of the current Torrent, updated at every announce request.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Stats {
    pub interval: u32,
    pub leechers: u32,
    pub seeders: u32,
}

impl Torrent {
    pub fn new(disk_tx: mpsc::Sender<DiskMsg>, magnet: &str, download_dir: &str) -> Self {
        let magnet = get_magnet(magnet).unwrap_or_else(|_| {
            eprintln!("The magnet link is invalid, try another one.");
            std::process::exit(exitcode::USAGE)
        });

        if !Path::new(&download_dir).is_dir() {
            eprintln!("Your download_dir is not a directory! Did you forget to create it?");
            std::process::exit(exitcode::USAGE)
        }

        let xt = magnet.xt.clone().unwrap();
        let dn = magnet.dn.clone().unwrap_or("Unknown".to_string());

        let pieces = RwLock::new(Bitfield::default());
        let info = RwLock::new(Info::default().name(dn));
        let info_dict = RwLock::new(BTreeMap::<u32, Vec<u8>>::new());
        let tracker_ctx = Arc::new(TrackerCtx::default());
        let requested_blocks = RwLock::new(VecDeque::new());
        let downloaded_blocks = RwLock::new(VecDeque::new());
        let block_infos = RwLock::new(VecDeque::new());

        let info_hash = get_info_hash(&xt);
        let (tx, rx) = mpsc::channel::<TorrentMsg>(300);

        let ctx = Arc::new(TorrentCtx {
            status: RwLock::new(TorrentStatus::default()),
            tracker_tx: RwLock::new(None),
            stats: RwLock::new(Stats::default()),
            info_hash,
            info_dict,
            pieces,
            requested_blocks,
            block_infos,
            magnet,
            downloaded_blocks,
            info,
            uploaded: Arc::new(AtomicU64::new(0)),
            downloaded: Arc::new(AtomicU64::new(0)),
        });

        Self {
            tracker_ctx,
            tracker_tx: None,
            ctx,
            in_end_game: false,
            disk_tx,
            tx,
            rx,
            peer_ctxs: Vec::new(),
        }
    }

    /// Start the Torrent, by sending `connect` and `announce_exchange`
    /// messages to one of the trackers, and returning a list of peers.
    #[tracing::instrument(skip(self), name = "torrent::start")]
    pub async fn start(&mut self, listen: Option<SocketAddr>) -> Result<Vec<Peer>, Error> {
        let mut tracker = Tracker::connect(self.ctx.magnet.tr.clone()).await?;
        let info_hash = self.ctx.clone().info_hash;
        let (res, peers) = tracker.announce_exchange(info_hash, listen).await?;

        let mut stats = self.ctx.stats.write().await;

        *stats = Stats {
            interval: res.interval,
            seeders: res.seeders,
            leechers: res.leechers,
        };
        let mut status = self.ctx.status.write().await;
        *status = TorrentStatus::Downloading;
        drop(status);

        info!("new stats {stats:#?}");
        drop(stats);

        let peers: Vec<Peer> = peers
            .into_iter()
            .map(|addr| {
                let (peer_tx, peer_rx) = mpsc::channel::<PeerMsg>(300);
                let torrent_ctx = self.ctx.clone();
                let torrent_tx = self.tx.clone();
                let tracker_ctx = self.tracker_ctx.clone();
                let disk_tx = self.disk_tx.clone();

                Peer::new(peer_tx, torrent_tx, torrent_ctx, peer_rx)
                    .addr(addr)
                    .tracker_ctx(tracker_ctx)
                    .disk_tx(disk_tx)
            })
            .collect();

        info!("tracker.ctx peer {:?}", self.tracker_ctx.local_peer_addr);

        self.tracker_ctx = tracker.ctx.clone().into();
        self.tracker_tx = Some(tracker.tx.clone());
        let mut ctx_tracker_tx = self.ctx.tracker_tx.write().await;
        *ctx_tracker_tx = Some(tracker.tx.clone());
        drop(ctx_tracker_tx);

        self.tracker_tx = Some(tracker.tx.clone());

        spawn(async move {
            let _ = tracker.run().await?;
            Ok::<(), Error>(())
        });

        Ok(peers)
    }

    #[tracing::instrument(skip(self), name = "torrent::start_and_run")]
    pub async fn start_and_run(&mut self, listen: Option<SocketAddr>) -> Result<(), Error> {
        let peers = self.start(listen).await?;

        self.spawn_outbound_peers(peers).await?;
        self.spawn_inbound_peers().await?;
        self.run().await?;

        Ok(())
    }

    /// Spawn an event loop for each peer to listen/send messages.
    pub async fn spawn_outbound_peers(&mut self, peers: Vec<Peer>) -> Result<(), Error> {
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
        let stats = self.ctx.stats.read().await;
        let tracker_tx = self.tracker_tx.clone().unwrap();

        let mut announce_interval = interval_at(
            Instant::now() + Duration::from_secs(stats.interval.max(500).into()),
            Duration::from_secs((stats.interval as u64).max(500)),
        );
        drop(stats);

        let mut request_interval = interval_at(
            Instant::now() + Duration::from_secs(10),
            Duration::from_secs(10),
        );

        loop {
            select! {
                Some(msg) = self.rx.recv() => {
                    match msg {
                        TorrentMsg::UpdateBitfield(len) => {
                            // create an empty bitfield with the same
                            // len as the bitfield from the peer
                            let ctx = Arc::clone(&self.ctx);
                            let mut pieces = ctx.pieces.write().await;

                            // only create the bitfield if we don't have one
                            // pieces.len() will start at 0
                            if pieces.len() < len {
                                let inner = vec![0_u8; len];
                                *pieces = Bitfield::from(inner);
                            }
                        }
                        TorrentMsg::DownloadedPiece(piece) => {
                            // send Have messages to peers that dont have our pieces
                            for peer in &self.peer_ctxs {
                                let _ = peer.peer_tx.send(PeerMsg::DownloadedPiece(piece)).await;
                            }
                        }
                        TorrentMsg::PeerConnected(ctx) => {
                            info!("connected with new peer");
                            self.peer_ctxs.push(ctx);
                        }
                        TorrentMsg::DownloadComplete => {
                            info!("received msg download complete");
                            let (otx, orx) = oneshot::channel();
                            let downloaded = self.ctx.downloaded.load(Ordering::Relaxed);
                            let uploaded = self.ctx.uploaded.load(Ordering::Relaxed);

                            let _ = tracker_tx.send(
                                TrackerMsg::Announce {
                                    event: Event::Completed,
                                    info_hash: self.ctx.info_hash,
                                    downloaded,
                                    uploaded,
                                    left: 0,
                                    recipient: Some(otx),
                                })
                            .await;

                            if let Ok(Ok(r)) = orx.await {
                                info!("announced completion with success {r:#?}");
                            }

                            // tell all peers that we are not interested,
                            // we wont request blocks from them anymore
                            for peer in &self.peer_ctxs {
                                let _ = peer.peer_tx.send(PeerMsg::NotInterested).await;
                            }

                            // announce to tracker that we are stopping
                            if Args::parse().quit_after_complete {
                                info!("exiting...");

                                let (otx, orx) = oneshot::channel();
                                let info = self.ctx.info.read().await;
                                let left = downloaded - info.get_size();

                                let _ = tracker_tx.send(
                                    TrackerMsg::Announce {
                                        event: Event::Stopped,
                                        info_hash: self.ctx.info_hash,
                                        downloaded,
                                        uploaded,
                                        left,
                                        recipient: Some(otx),
                                    })
                                .await;

                                if let Ok(Ok(r)) = orx.await {
                                    info!("announced stopped with success {r:#?}");
                                }

                                // let _ = self.front_tx.send();
                                // std::process::exit(exitcode::OK);
                            }
                        }
                    }
                }
                // Sometimes that blocks requested are never sent to us,
                // The algorithm to re-request those blocks is simple:
                // At every 10 seconds, check for blocks that were requested,
                // but not downloaded, and request them again.
                _ = request_interval.tick() => {
                    let requested = self.ctx.requested_blocks.read().await;
                    let downloaded = self.ctx.downloaded_blocks.read().await;

                    'outer: for req in requested.iter() {
                        let not_downloaded = !downloaded.iter().any(|b| *b == *req);
                        // get a block that was requested but not downloaded
                        if not_downloaded {
                            // get the first peer that has this piece
                            for peer in &self.peer_ctxs {
                                let pieces = peer.pieces.read().await;
                                let bit_item = pieces.get(req.index as usize);
                                let am_interested = peer.am_interested.load(Ordering::Relaxed);
                                let peer_choking = peer.peer_choking.load(Ordering::Relaxed);

                                // and we are interested and not choked
                                match bit_item {
                                    Some(bit_item) if bit_item.bit == 1 && am_interested && !peer_choking => {
                                        let _ = peer.peer_tx.send(PeerMsg::RequestInfo(req.clone())).await;
                                        // continue to the next requested block,
                                        // we only want to send one unique block
                                        // to each peer
                                        continue 'outer;
                                    }
                                    _ => {},
                                }
                            }
                        }
                    }
                    drop(requested);
                    drop(downloaded);
                }
                // periodically announce to tracker, at the specified interval
                // to update the tracker about the client's stats.
                // let have_info = self.ctx.info_dict;
                _ = announce_interval.tick() => {
                    let info = self.ctx.info.read().await;
                    // we know if the info is downloaded if the piece_length is > 0
                    if info.piece_length > 0 {
                        info!("sending periodic announce, interval {announce_interval:?}");
                        let db = self.ctx.downloaded_blocks.read().await;
                        let downloaded = db.iter().fold(0, |acc, x| acc + x.len as u64);
                        let left = if downloaded < info.get_size() {info.get_size()} else { downloaded - info.get_size()};
                        drop(db);
                        let (otx, orx) = oneshot::channel();

                        let _ = tracker_tx.send(
                            TrackerMsg::Announce {
                                event: Event::None,
                                info_hash: self.ctx.info_hash,
                                downloaded,
                                uploaded: self.ctx.uploaded.load(Ordering::SeqCst),
                                left,
                                recipient: Some(otx),
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
                            // announce_interval = interval(Duration::from_secs(r.interval as u64));
                            announce_interval = interval_at(
                                Instant::now() + Duration::from_secs(r.interval as u64),
                                Duration::from_secs(r.interval as u64),
                            );
                        }
                    }
                    drop(info);
                }
            }
        }
    }
}
