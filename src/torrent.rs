use crate::magnet_parser::get_magnet;
use crate::peer::session::ConnectionState;
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
use hashbrown::{HashMap, HashSet};
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
    PeerConnected([u8; 20], Arc<PeerCtx>),
    DownloadComplete,
    /// When in endgame mode, the first peer that receives this info,
    /// sends this message to send Cancel's to all other peers.
    SendCancel {
        from: [u8; 20],
        block_info: BlockInfo,
    },
    /// When a peer is Choked, or receives an error and must close the connection,
    /// the outgoing/pending blocks of this peer must be appended back
    /// to the list of available block_infos.
    ReturnBlockInfo(BlockInfo),
    StartEndgame([u8; 20], BlockInfo),
    /// When torrent is being gracefully shutdown
    Quit,
}

/// This is the main entity responsible for the high-level management of
/// a torrent download or upload.
#[derive(Debug)]
pub struct Torrent {
    pub ctx: Arc<TorrentCtx>,
    pub tracker_ctx: Arc<TrackerCtx>,
    pub disk_tx: mpsc::Sender<DiskMsg>,
    pub rx: mpsc::Receiver<TorrentMsg>,
    pub peer_ctxs: HashMap<[u8; 20], Arc<PeerCtx>>,
    pub tracker_tx: Option<mpsc::Sender<TrackerMsg>>,
}

#[derive(Debug)]
pub struct TorrentCtx {
    pub tx: mpsc::Sender<TorrentMsg>,
    pub tracker_tx: RwLock<Option<mpsc::Sender<TrackerMsg>>>,
    pub magnet: Magnet,
    pub info_hash: [u8; 20],
    pub pieces: RwLock<Bitfield>,
    /// The block infos of this torrent to be downloaded,
    /// when a block info is downloaded, it is moved from this Vec,
    /// to [`downloaded_blocks`].
    pub block_infos: RwLock<VecDeque<BlockInfo>>,
    pub downloaded_blocks: RwLock<HashSet<BlockInfo>>,
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
    /// The downloaded bytes of the previous second,
    /// used to get the download rate in seconds.
    /// this will be mutated on the frontend event loop.
    pub last_second_downloaded: Arc<AtomicU64>,
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
        let downloaded_blocks = RwLock::new(HashSet::new());
        let block_infos = RwLock::new(VecDeque::new());

        let info_hash = get_info_hash(&xt);
        let (tx, rx) = mpsc::channel::<TorrentMsg>(300);

        let ctx = Arc::new(TorrentCtx {
            tx: tx.clone(),
            status: RwLock::new(TorrentStatus::default()),
            tracker_tx: RwLock::new(None),
            stats: RwLock::new(Stats::default()),
            info_hash,
            info_dict,
            pieces,
            block_infos,
            magnet,
            downloaded_blocks,
            info,
            uploaded: Arc::new(AtomicU64::new(0)),
            downloaded: Arc::new(AtomicU64::new(0)),
            last_second_downloaded: Arc::new(AtomicU64::new(0)),
        });

        Self {
            tracker_ctx,
            tracker_tx: None,
            ctx,
            disk_tx,
            rx,
            peer_ctxs: HashMap::new(),
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
                let tracker_ctx = self.tracker_ctx.clone();
                let disk_tx = self.disk_tx.clone();

                Peer::new(addr, peer_tx, torrent_ctx, peer_rx, disk_tx, tracker_ctx)
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
            peer.session.state.connection = ConnectionState::Connecting;

            // send connections too other peers
            spawn(async move {
                match TcpStream::connect(peer.addr).await {
                    Ok(socket) => {
                        info!("we connected with {:?}", peer.addr);

                        let socket = Framed::new(socket, HandshakeCodec);
                        let socket = peer.start(Direction::Outbound, socket).await?;
                        let r = peer.run(Direction::Outbound, socket).await;

                        if let Err(r) = r {
                            warn!("Peer session stopped due to an error: {}", r);
                        }
                    }
                    Err(e) => {
                        warn!("error with peer: {:?} {e:#?}", peer.addr);
                    }
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
                Ok::<(), Error>(())
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

        let torrent_ctx = Arc::clone(&self.ctx);
        let tracker_ctx = self.tracker_ctx.clone();
        let disk_tx = self.disk_tx.clone();

        // accept connections from other peers
        spawn(async move {
            info!("accepting requests in {local_peer_socket:?}");

            loop {
                if let Ok((socket, addr)) = local_peer_socket.accept().await {
                    let socket = Framed::new(socket, HandshakeCodec);

                    info!("received inbound connection from {addr}");

                    let (peer_tx, peer_rx) = mpsc::channel::<PeerMsg>(300);

                    let mut peer = Peer::new(
                        addr,
                        peer_tx,
                        torrent_ctx.clone(),
                        peer_rx,
                        disk_tx.clone(),
                        tracker_ctx.clone(),
                    );

                    spawn(async move {
                        peer.session.state.connection = ConnectionState::Connecting;
                        let socket = peer.start(Direction::Inbound, socket).await?;

                        let r = peer.run(Direction::Inbound, socket).await;

                        if let Err(r) = r {
                            warn!("Peer session stopped due to an error: {}", r);
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
                            for peer in self.peer_ctxs.values() {
                                let _ = peer.tx.send(PeerMsg::DownloadedPiece(piece)).await;
                            }
                        }
                        TorrentMsg::PeerConnected(id, ctx) => {
                            info!("connected with new peer");
                            self.peer_ctxs.insert(id, ctx);
                        }
                        TorrentMsg::DownloadComplete => {
                            info!("received msg download complete");
                            let (otx, orx) = oneshot::channel();
                            let downloaded = self.ctx.downloaded.load(Ordering::Relaxed);
                            let uploaded = self.ctx.uploaded.load(Ordering::Relaxed);

                            let mut status = self.ctx.status.write().await;
                            *status = TorrentStatus::Seeding;
                            drop(status);

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
                            for peer in self.peer_ctxs.values() {
                                let _ = peer.tx.send(PeerMsg::NotInterested).await;
                            }

                            //
                            // announce to tracker that we are stopping
                            if Args::parse().quit_after_complete {
                                let _ = self.ctx.tx.send(TorrentMsg::Quit).await;
                            }
                        }
                        // The peer "from" was the first one to receive the "info".
                        // Send Cancel messages to everyone else.
                        TorrentMsg::SendCancel { from, block_info } => {
                            for (k, peer) in self.peer_ctxs.iter() {
                                if *k == from { continue };
                                let _ = peer.tx.send(PeerMsg::Cancel(block_info.clone())).await;
                            }
                        }
                        TorrentMsg::ReturnBlockInfo(block_info) => {
                            let mut blocks = self.ctx.block_infos.write().await;
                            blocks.push_front(block_info);
                        }
                        TorrentMsg::StartEndgame(peer_id, block_info) => {
                            let peer = self.peer_ctxs.get(&peer_id);
                            if let Some(peer) = peer {
                                let _ = peer.tx.send(PeerMsg::RequestBlockInfo(block_info)).await;
                            }
                        }
                        TorrentMsg::Quit => {
                            info!("torrent is quitting");
                            let (otx, orx) = oneshot::channel();
                            let downloaded = self.ctx.downloaded.load(Ordering::Relaxed);
                            let uploaded = self.ctx.uploaded.load(Ordering::Relaxed);
                            let info = self.ctx.info.read().await;
                            let left =
                                if downloaded > info.get_size()
                                    { downloaded - info.get_size() }
                                else { 0 };

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

                            for peer in self.peer_ctxs.values() {
                                let tx = peer.tx.clone();
                                spawn(async move {
                                    let _ = tx.send(PeerMsg::Quit).await;
                                });
                            }

                            if let Ok(Ok(r)) = orx.await {
                                info!("announced stopped with success {r:#?}");
                            }

                            return Ok(());
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
