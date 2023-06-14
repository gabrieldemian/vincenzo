use crate::bitfield::Bitfield;
use crate::error::Error;
use crate::magnet_parser::get_info_hash;
use crate::peer::Peer;
use crate::tracker::tracker::Tracker;
use crate::tracker::tracker::TrackerCtx;
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
    pub magnet: Magnet,
    pub requested_pieces: Arc<RwLock<Bitfield>>,
}

impl Torrent {
    pub async fn new(tx: Sender<TorrentMsg>, rx: Receiver<TorrentMsg>, magnet: Magnet) -> Self {
        let requested_pieces = Arc::new(RwLock::new(Bitfield::default()));

        let tracker_ctx = Arc::new(TrackerCtx::default());
        let ctx = Arc::new(TorrentCtx {
            requested_pieces,
            magnet,
        });

        Self {
            tracker_ctx,
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

    /// Spawn an event loop for each peer to listen/send messages.
    pub async fn spawn_peers_tasks(&self, peers: Vec<Peer>) -> Result<(), Error> {
        for mut peer in peers {
            peer.torrent_ctx = Some(Arc::clone(&self.ctx));
            peer.tracker_ctx = Arc::clone(&self.tracker_ctx);

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

        Ok(peers)
    }
}

// #[cfg(test)]
// mod tests {
//     use super::*;
//
//     #[test]
//     fn peers() {
//         let peer_1 = "";
//     }
// }
