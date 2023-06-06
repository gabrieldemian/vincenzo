use crate::error::Error;
use crate::magnet_parser::get_info_hash;
use crate::tcp_wire::messages::Handshake;
use crate::tcp_wire::messages::HaveNone;
use crate::tcp_wire::messages::Interested;
use crate::tcp_wire::messages::Unchoke;
use crate::tracker::tracker::Tracker;
use log::debug;
use log::info;
use log::warn;
use magnet_url::Magnet;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::spawn;
use tokio::sync::mpsc::Receiver;
use tokio::sync::mpsc::Sender;
use tokio::time::interval;
use tokio::time::timeout;
use tokio::time::Interval;

#[derive(Debug)]
pub enum TorrentMsg {
    AddMagnet(Magnet),
    ConnectedPeer(SocketAddr),
}

#[derive(Debug)]
pub struct Torrent {
    pub peers: Vec<SocketAddr>,
    pub tx: Sender<TorrentMsg>,
    pub rx: Receiver<TorrentMsg>,
    pub tick_interval: Interval,
}

impl Torrent {
    pub async fn new(tx: Sender<TorrentMsg>, rx: Receiver<TorrentMsg>) -> Self {
        let peers = vec![];

        Self {
            peers,
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
                match msg {
                    TorrentMsg::AddMagnet(link) => {
                        self.add_magnet(link).await.unwrap();
                    }
                    TorrentMsg::ConnectedPeer(addr) => {
                        // this peer has been handshake'd
                        // and is ready to send/receive msgs
                        info!("listening to msgs from {:?}", addr);
                    }
                }
            }
        }
    }

    // each connected peer has its own event loop
    // cant call this inside any other async loop, it must be called
    // as close as possible to the first tokio task to prevent memory leaks
    pub async fn listen_to_peers(peers: Vec<SocketAddr>, our_handshake: Handshake) {
        for peer in peers {
            let our_handshake = our_handshake.clone();
            debug!("listening to peer...");

            let mut tick_timer = interval(Duration::from_secs(1));

            spawn(async move {
                let socket = TcpStream::connect(peer).await;

                if let Err(_) = socket {
                    return;
                }

                let mut socket = socket.unwrap();
                let (mut rd, mut wt) = socket.split();

                if let Err(_) = timeout(
                    Duration::new(3, 0),
                    wt.write_all(&our_handshake.serialize().unwrap()),
                )
                .await
                {
                    return;
                }

                // let mut buf = vec![];
                // if let Err(_) = timeout(Duration::new(3, 0), socket.read_to_end(&mut buf)).await {
                //     return;
                // }
                // let their_handshake = Handshake::deserialize(&buf);
                // if let Err(_) = their_handshake {
                //     return;
                // }
                // let their_handshake = their_handshake.unwrap();
                // info!("received handshake {:?}", their_handshake);

                // a Vec will never read anything, the buffer
                // must always be a fixed length slice
                let mut buf = [0; 1024];

                loop {
                    tick_timer.tick().await;
                    match rd.read(&mut buf).await {
                        // Ok(0) means that the stream has closed,
                        // the peer is not listening to us anymore
                        Ok(0) => {
                            warn!("peer {:?} closed connection", peer);
                            // cancel task
                            break;
                        }
                        Ok(n) => {
                            info!("!!!!!! received something from peer {:?}", &buf[..n]);
                        }
                        Err(e) => warn!("error reading peer loop {:?}", e),
                    };
                }
            });
        }
    }

    async fn add_magnet(&self, m: Magnet) -> Result<(), Error> {
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
        let tx = self.tx.clone();

        // spawn tracker event loop
        spawn(async move {
            tracker.run(tx.clone()).await;
        });

        info!("sending handshake req to {:?} peers...", peers.len());

        let our_handshake = Handshake::new(info_hash, peer_id);

        // reserved: [0, 0, 0, 0, 0, 16, 0, 5]
        // each peer will have its own event loop
        Torrent::listen_to_peers(peers, our_handshake).await;
        Ok(())
    }
}

//
// this is probably a bitfield message
//
// [0, 0, 0, 1, 0]
// [0, 0, 0, 1, 1]
// [0, 0, 0, 1, 0]
// [0, 0, 0, 1, 0]
//
//[255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 252]
//
//
// [255, 175, 123, 255, 254, 255, 191, 127, 255, 255, 255, 255, 255, 251, 251, 255, 247, 255, 255, 239, 254, 255, 255, 255, 255, 191, 255, 255, 255, 255, 255, 223, 255, 255, 255, 255, 255, 255, 255, 255, 252, 0, 0, 0, 5, 4, 0, 0, 0, 236, 0, 0, 0, 5, 4, 0, 0, 0, 30, 0, 0, 0, 5, 4, 0, 0, 0, 167, 0, 0, 0, 5, 4, 0, 0, 0, 249, 0, 0, 0, 5, 4, 0, 0, 1, 93, 0, 0, 0, 5, 4, 0, 0, 1, 5, 0, 0, 0, 5, 4, 0, 0, 0, 77, 0, 0, 0, 5, 4, 0, 0, 1, 151, 0, 0, 0, 5, 4, 0, 0, 1, 116, 0, 0, 0, 5, 4, 0, 0, 1, 23, 0, 0, 0, 5, 4, 0, 0, 1, 40, 0, 0, 0, 5, 4, 0, 0, 0, 228, 0, 0, 0, 5, 4, 0, 0, 1, 101, 0, 0, 0, 5, 4, 0, 0, 1, 234, 0, 0, 0, 5, 4, 0, 0, 1, 33, 0, 0, 0, 5, 4, 0, 0, 1, 139, 0, 0, 0, 5, 4, 0, 0, 0, 172, 0, 0, 0, 5, 4, 0, 0, 0, 50, 0, 0, 0, 5, 4, 0, 0, 0, 91, 0, 0, 0, 5, 4, 0, 0, 0, 238, 0, 0, 0, 5, 4, 0, 0, 1, 0, 0, 0, 0, 5, 4, 0, 0, 0, 100, 0, 0, 0, 5, 4, 0, 0, 0, 251, 0, 0, 0, 5, 4, 0, 0, 1, 185]
//
//
//[255, 255, 255, 255, 255, 255, 255, 255, 253, 255, 255, 238, 255, 255, 254, 255, 255, 255, 255, 255, 221, 255, 255, 255, 250, 255, 255, 247, 255, 255, 251, 255, 255, 255, 239, 255, 255, 255, 255, 247, 255, 191, 253, 252, 0, 0, 0, 5, 4, 0, 0, 1, 235, 0, 0, 0, 5, 4, 0, 0, 0, 41, 0, 0, 0, 5, 4, 0, 0, 1, 30, 0, 0, 0, 5, 4, 0, 0, 1, 157, 0, 0, 0, 5, 4, 0, 0, 0, 210, 0, 0, 0, 5, 4, 0, 0, 1, 122, 0, 0, 0, 5, 4, 0, 0, 1, 205, 0, 0, 0, 5, 4, 0, 0, 0, 214, 0, 0, 0, 5, 4, 0, 0, 0, 49, 0, 0, 0, 5, 4, 0, 0, 0, 195, 0, 0, 0, 5, 4, 0, 0, 1, 51, 0, 0, 0, 5, 4, 0, 0, 0, 174, 0, 0, 0, 5, 4, 0, 0, 0, 72, 0, 0, 0, 5, 4, 0, 0, 2, 33, 0, 0, 0, 5, 4, 0, 0, 1, 180, 0, 0, 0, 5, 4, 0, 0, 2, 20, 0, 0, 0, 5, 4, 0, 0, 1, 159, 0, 0, 0, 5, 4, 0, 0, 2, 46, 0, 0, 0, 5, 4, 0, 0, 0, 145, 0, 0, 0, 5, 4, 0, 0, 1, 55, 0, 0, 0, 5, 4, 0, 0, 1, 126, 0, 0, 0, 5, 4, 0, 0, 1, 79, 0, 0, 0, 5, 4, 0, 0, 0, 109, 0, 0, 0, 5, 4, 0, 0, 0, 90]
//
//
//[127, 251, 255, 191, 255, 255, 247, 255, 255, 255, 255, 175, 255, 223, 255, 255, 255, 255, 251, 223, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 252, 0, 0, 0, 5, 4, 0, 0, 0, 4, 0, 0, 0, 5, 4, 0, 0, 0, 130, 0, 0, 0, 5, 4, 0, 0, 0, 37, 0, 0, 0, 5, 4, 0, 0, 1, 7, 0, 0, 0, 5, 4, 0, 0, 0, 35, 0, 0, 0, 5, 4, 0, 0, 0, 96, 0, 0, 0, 5, 4, 0, 0, 0, 217, 0, 0, 0, 5, 4, 0, 0, 1, 123, 0, 0, 0, 5, 4, 0, 0, 0, 230, 0, 0, 0, 5, 4, 0, 0, 1, 121, 0, 0, 0, 5, 4, 0, 0, 1, 45, 0, 0, 0, 5, 4, 0, 0, 0, 158, 0, 0, 0, 5, 4, 0, 0, 1, 181, 0, 0, 0, 5, 4, 0, 0, 0, 170, 0, 0, 0, 5, 4, 0, 0, 1, 138, 0, 0, 0, 5, 4, 0, 0, 0, 58, 0, 0, 0, 5, 4, 0, 0, 1, 32, 0, 0, 0, 5, 4, 0, 0, 0, 202, 0, 0, 0, 5, 4, 0, 0, 1, 57, 0, 0, 0, 5, 4, 0, 0, 0, 80, 0, 0, 0, 5, 4, 0, 0, 0, 200, 0, 0, 0, 5, 4, 0, 0, 0, 191, 0, 0, 0, 5, 4, 0, 0, 1, 84, 0, 0, 0, 5, 4, 0, 0, 1, 186]
