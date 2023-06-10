use crate::bitfield::Bitfield;
use crate::error::Error;
use crate::magnet_parser::get_info_hash;
use crate::peer::Peer;
use crate::tcp_wire::messages::Handshake;
use crate::tracker::tracker::Tracker;
use log::debug;
use log::info;
use magnet_url::Magnet;
use std::sync::Arc;
use std::time::Duration;
use tokio::spawn;
use tokio::sync::mpsc::Receiver;
use tokio::sync::mpsc::Sender;
use tokio::sync::RwLock;
use tokio::time::interval;
use tokio::time::Interval;

#[derive(Debug)]
pub enum TorrentMsg {
    AddMagnet(Magnet),
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
    pub bitfield: Bitfield,
    pub tx: Sender<TorrentMsg>,
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
    pub requested_pieces: Arc<RwLock<Vec<u32>>>,
}

impl Torrent {
    pub async fn new(tx: Sender<TorrentMsg>, rx: Receiver<TorrentMsg>) -> Self {
        let requested_pieces = Arc::new(RwLock::new(Vec::new()));

        let ctx = Arc::new(TorrentCtx { requested_pieces });

        Self {
            ctx,
            bitfield: Bitfield::default(),
            in_end_game: false,
            tx,
            rx,
            tick_interval: interval(Duration::new(1, 0)),
        }
    }

    pub async fn run(&mut self) -> Result<(), Error> {
        loop {
            self.tick_interval.tick().await;
            debug!("tick torrent");
            if let Ok(msg) = self.rx.try_recv() {
                // in the future, this event loop will
                // send messages to the frontend,
                // the terminal ui.
                match msg {
                    TorrentMsg::AddMagnet(link) => {
                        self.add_magnet(link).await.unwrap();
                    }
                    TorrentMsg::UpdateBitfield(len) => {
                        // create an empty bitfield with the same
                        // len as the bitfield from the peer
                        let inner = vec![0_u8; len];
                        self.bitfield = Bitfield::from(inner);
                    }
                }
            }
        }
    }

    /// each connected peer has its own event loop
    pub async fn spawn_peers_tasks(
        &self,
        peers: Vec<Peer>,
        our_handshake: Handshake,
    ) -> Result<(), Error> {
        for mut peer in peers {
            let tx = self.tx.clone();
            let ctx = Arc::clone(&self.ctx);
            let our_handshake = our_handshake.clone();

            debug!("listening to peer...");

            spawn(async move {
                peer.run(our_handshake, ctx, tx).await?;
                Ok::<_, Error>(())
            });
        }
        Ok(())
    }

    pub async fn add_magnet(&mut self, m: Magnet) -> Result<(), Error> {
        debug!("{:#?}", m);
        info!("received add_magnet call");
        let info_hash = get_info_hash(&m.xt.unwrap());
        debug!("info_hash {:?}", info_hash);

        // first, do a `connect` handshake to the tracker
        let tracker = Tracker::connect(m.tr).await?;
        let peer_id = tracker.peer_id;

        // second, do a `announce` handshake to the tracker
        // and get the list of peers for this torrent
        let peers = tracker.announce_exchange(info_hash).await?;

        // listen to events on our peer socket,
        // that we used to announce to trackers.
        // spawn tracker event loop
        // let tx = self.tx.clone();
        // spawn(async move {
        //     tracker.run(tx).await;
        // });

        info!("sending handshake req to {:?} peers...", peers.len());

        let our_handshake = Handshake::new(info_hash, peer_id);

        // each peer will have its own event loop
        self.spawn_peers_tasks(peers, our_handshake).await?;

        Ok(())
    }
}
