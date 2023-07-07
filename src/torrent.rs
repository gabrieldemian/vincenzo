use crate::bitfield::Bitfield;
use crate::cli::Args;
use crate::disk::DiskMsg;
use crate::error::Error;
use crate::magnet_parser::get_info_hash;
use crate::metainfo::Info;
use crate::peer::Direction;
use crate::peer::Peer;
use crate::tcp_wire::lib::BlockInfo;
use crate::tcp_wire::messages::HandshakeCodec;
use crate::tracker::tracker::Tracker;
use crate::tracker::tracker::TrackerCtx;
use clap::Parser;
use magnet_url::Magnet;
use tokio::net::TcpListener;
use tokio::net::TcpStream;
use tokio_util::codec::Framed;
use tracing::warn;

use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;
use tokio::select;
use tokio::spawn;
use tokio::sync::mpsc;
use tokio::sync::RwLock;
use tokio::time::interval;
use tokio::time::Interval;
use tracing::debug;
use tracing::info;

#[derive(Debug)]
pub enum TorrentMsg {
    // Torrent will start with a blank bitfield
    // because it cannot know it from a magnet link
    // once a peer send the first bitfield message,
    // we will update the torrent bitfield
    UpdateBitfield(usize),
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
    pub tick_interval: Interval,
    pub in_end_game: bool,
}

/// Information and methods shared with peer sessions in the torrent.
/// This type contains fields that need to be read or updated by peer sessions.
/// Fields expected to be mutated are thus secured for inter-task access with
/// various synchronization primitives.
#[derive(Debug)]
pub struct TorrentCtx {
    pub magnet: Magnet,
    pub pieces: RwLock<Bitfield>,
    pub requested_blocks: RwLock<VecDeque<BlockInfo>>,
    pub downloaded_blocks: RwLock<VecDeque<BlockInfo>>,
    pub info: RwLock<Info>,
    // If using a Magnet link, the info will be downloaded in pieces
    // and those pieces may come in different order,
    // hence the HashMap (dictionary), and not a vec.
    // After the dict is complete, it will be decoded into "info"
    pub info_dict: RwLock<HashMap<u32, Vec<u8>>>,
}

impl Torrent {
    pub async fn new(
        tx: mpsc::Sender<TorrentMsg>,
        disk_tx: mpsc::Sender<DiskMsg>,
        rx: mpsc::Receiver<TorrentMsg>,
        magnet: Magnet,
    ) -> Self {
        let pieces = RwLock::new(Bitfield::default());
        let info = RwLock::new(Info::default());
        let info_dict = RwLock::new(HashMap::<u32, Vec<u8>>::new());
        let tracker_ctx = Arc::new(TrackerCtx::default());
        let requested_blocks = RwLock::new(VecDeque::new());
        let downloaded_blocks = RwLock::new(VecDeque::new());

        let ctx = Arc::new(TorrentCtx {
            info_dict,
            pieces,
            requested_blocks,
            magnet,
            downloaded_blocks,
            info,
        });

        Self {
            tracker_ctx,
            ctx,
            in_end_game: false,
            disk_tx,
            tx,
            rx,
            tick_interval: interval(Duration::new(1, 0)),
        }
    }

    #[tracing::instrument(name = "torrent::run", skip(self))]
    pub async fn run(&mut self) -> Result<(), Error> {
        loop {
            select! {
                _ = self.tick_interval.tick() => {
                    let downloaded = self.ctx.downloaded_blocks.read().await;
                    let downloaded = downloaded.len();
                    let blocks_len = self.ctx.info.read().await.blocks_len();

                    if (downloaded as u32) >= blocks_len && downloaded > 0 && blocks_len > 0 {
                        let args = Args::parse();

                        info!("torrent downloaded fully");

                        if args.quit_after_complete {
                            info!("exiting...");
                            std::process::exit(exitcode::OK);
                        }
                    }
                },
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
                    }
                }
            }
        }
    }

    /// Spawn an event loop for each peer to listen/send messages.
    pub async fn spawn_outbound_peers(&self, peers: Vec<Peer>) -> Result<(), Error> {
        for mut peer in peers {
            peer.torrent_ctx = Some(Arc::clone(&self.ctx));
            peer.tracker_ctx = Arc::clone(&self.tracker_ctx);
            peer.disk_tx = Some(self.disk_tx.clone());

            let tx = self.tx.clone();

            info!("outbound peer {:?}", peer.addr);

            // send connections too other peers
            spawn(async move {
                match TcpStream::connect(peer.addr).await {
                    Ok(socket) => {
                        let socket = Framed::new(socket, HandshakeCodec);
                        info!("we connected with {:?}", peer.addr);
                        let _ = peer.run(tx, Direction::Outbound, socket).await;
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
        let local_peer_socket = TcpListener::bind(self.tracker_ctx.local_peer_addr).await?;

        let tx = self.tx.clone();
        let disk_tx = self.disk_tx.clone();
        let torrent_ctx = Some(Arc::clone(&self.ctx));
        let tracker_ctx = Arc::clone(&self.tracker_ctx);

        // accept connections from other peers
        spawn(async move {
            loop {
                if let Ok((socket, addr)) = local_peer_socket.accept().await {
                    let socket = Framed::new(socket, HandshakeCodec);

                    info!("received inbound connection from {addr}");

                    let tx = tx.clone();
                    let mut peer: Peer = addr.into();

                    peer.torrent_ctx = torrent_ctx.clone();
                    peer.tracker_ctx = tracker_ctx.clone();
                    peer.disk_tx = Some(disk_tx.clone());

                    spawn(async move {
                        peer.run(tx, Direction::Inbound, socket).await?;
                        Ok::<(), Error>(())
                    });
                }
            }
        });

        Ok(())
    }

    /// Start the Torrent, by sending `connect` and `announce_exchange`
    /// messages to one of the trackers, and returning a list of peers.
    #[tracing::instrument(skip(self), name = "torrent::start")]
    pub async fn start(&mut self) -> Result<Vec<Peer>, Error> {
        debug!("{:#?}", self.ctx.magnet);
        info!("received add_magnet call");

        let xt = self.ctx.magnet.xt.clone().unwrap();
        let info_hash = get_info_hash(&xt);

        debug!("info_hash {:?}", info_hash);

        let args = Args::parse();
        let mut peers: Vec<Peer> = Vec::new();
        let mut tracker = Tracker::default();

        match args.seeds {
            Some(seeds) => {
                let peers_l: Vec<Peer> = seeds.into_iter().map(|p| p.into()).collect();
                peers.extend_from_slice(&peers_l);
            }
            None => {
                let tracker_l = Tracker::connect(self.ctx.magnet.tr.clone()).await?;
                tracker = tracker_l;

                let peers_l = tracker.announce_exchange(info_hash).await?;
                peers = peers_l;
            }
        };

        self.tracker_ctx = Arc::new(tracker.ctx);
        self.spawn_inbound_peers().await?;

        Ok(peers)
    }
}
