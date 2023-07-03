use crate::bitfield::Bitfield;
use crate::disk::DiskMsg;
use crate::error::Error;
use crate::magnet_parser::get_info_hash;
use crate::metainfo::Info;
use crate::metainfo::MetaInfo;
use crate::peer::Peer;
use crate::tcp_wire::lib::BlockInfo;
use crate::tracker::tracker::Tracker;
use crate::tracker::tracker::TrackerCtx;
use bendy::decoding::FromBencode;
use log::debug;
use log::info;
use magnet_url::Magnet;
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;
use tokio::select;
use tokio::spawn;
use tokio::sync::mpsc::Receiver;
use tokio::sync::mpsc::Sender;
use tokio::sync::RwLock;
use tokio::time::interval;
use tokio::time::Interval;

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
    pub tx: Sender<TorrentMsg>,
    pub disk_tx: Sender<DiskMsg>,
    pub rx: Receiver<TorrentMsg>,
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
}

impl Torrent {
    pub async fn new(
        tx: Sender<TorrentMsg>,
        disk_tx: Sender<DiskMsg>,
        rx: Receiver<TorrentMsg>,
        magnet: Magnet,
    ) -> Self {
        let pieces = RwLock::new(Bitfield::default());
        let info = RwLock::new(Info::default());
        let tracker_ctx = Arc::new(TrackerCtx::default());
        let requested_blocks = RwLock::new(VecDeque::new());
        let downloaded_blocks = RwLock::new(VecDeque::new());

        let ctx = Arc::new(TorrentCtx {
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

    pub async fn run(&mut self) -> Result<(), Error> {
        loop {
            select! {
                _ = self.tick_interval.tick() => {
                    let downloaded = self.ctx.downloaded_blocks.read().await;
                    let downloaded = downloaded.len();
                    let blocks_len = self.ctx.info.read().await.blocks_len();

                    if (downloaded as u32) == blocks_len {
                        println!("torrent downloaded fully");
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

                            println!("update bitfield len {:?}", len);

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
    pub async fn spawn_peers_tasks(&self, peers: Vec<Peer>) -> Result<(), Error> {
        for mut peer in peers {
            peer.torrent_ctx = Some(Arc::clone(&self.ctx));
            peer.tracker_ctx = Arc::clone(&self.tracker_ctx);
            peer.disk_tx = Some(self.disk_tx.clone());

            debug!("listening to peer...");

            let tx = self.tx.clone();

            spawn(async move {
                peer.run(tx, None).await?;
                Ok::<_, Error>(())
            });
        }
        Ok(())
    }

    /// Start the Torrent, by sending `connect` and `announce_exchange`
    /// messages to one of the trackers, and returning a list of peers.
    pub async fn start(&mut self) -> Result<Vec<Peer>, Error> {
        debug!("{:#?}", self.ctx.magnet);
        info!("received add_magnet call");

        let xt = self.ctx.magnet.xt.clone().unwrap();
        let info_hash = get_info_hash(&xt);

        debug!("info_hash {:?}", info_hash);

        // first, do a `connect` handshake to the tracker
        let tracker = Tracker::connect(self.ctx.magnet.tr.clone()).await?;
        // second, do a `announce` handshake to the tracker
        // and get the list of peers for this torrent
        let peers = tracker.announce_exchange(info_hash).await?;
        self.tracker_ctx = Arc::new(tracker.ctx);

        let local = tracker.local_addr;
        let peer = tracker.peer_addr;

        spawn(async move {
            Tracker::run(local, peer).await.unwrap();
        });

        // pretending we get this from bep 09 msg,
        // its not ready yet
        let torrent_book_bytes = include_bytes!("../book.torrent");
        let torrent = MetaInfo::from_bencode(torrent_book_bytes).unwrap();

        let ctx = self.ctx.clone();
        let mut info = ctx.info.write().await;

        *info = torrent.info.clone();

        // send this message here only if the user is using a 
        // metainfo instead of a magnet link
        // self.disk_tx
        //     .send(DiskMsg::NewTorrent(torrent.info))
        //     .await
        //     .unwrap();

        Ok(peers)
    }
}
