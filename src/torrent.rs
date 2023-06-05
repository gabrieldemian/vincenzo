use crate::error::Error;
use crate::magnet_parser::get_info_hash;
use crate::tcp_wire::messages::Handshake;
use crate::tcp_wire::messages::Interested;
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

#[derive(Debug)]
pub enum TorrentMsg {
    Quit,
    AddMagnet(Magnet),
    ConnectedPeer(SocketAddr),
}

#[derive(Debug)]
pub struct Torrent {
    pub peers: Vec<SocketAddr>,
    pub tx: Sender<TorrentMsg>,
    pub rx: Receiver<TorrentMsg>,
}

impl Torrent {
    pub async fn new(tx: Sender<TorrentMsg>, rx: Receiver<TorrentMsg>) -> Self {
        let peers = vec![];

        Self { peers, tx, rx }
    }

    pub async fn run(&mut self) -> Result<(), Error> {
        let mut tick_timer = interval(Duration::from_secs(1));

        loop {
            tick_timer.tick().await;
            debug!("tick torrent");
            if let Ok(msg) = self.rx.try_recv() {
                match msg {
                    TorrentMsg::Quit => {
                        // todo
                    }
                    TorrentMsg::AddMagnet(link) => {
                        let tx = self.tx.clone();
                        Torrent::add_magnet(link, tx).await.unwrap();
                    }
                    TorrentMsg::ConnectedPeer(addr) => {
                        // this peer has been handshake'd
                        // and is ready to send/receive msgs
                        info!("listening to msgs from {:?}", addr);
                        self.peers.push(addr);
                        self.listen_to_peers().await;
                    }
                }
            }
        }
    }

    // each connected peer has its own event loop
    // cant call this inside any other async loop, it must be called
    // as close as possible to the first tokio task to prevent memory leaks
    pub async fn listen_to_peers(&mut self) {
        info!("-- CALLED LISTEN TO PEERS --");

        self.peers.drain(0..).into_iter().for_each(|peer| {
            debug!("listening to peer...");

            let mut tick_timer = interval(Duration::from_secs(1));

            spawn(async move {
                let mut socket = TcpStream::connect(peer).await.unwrap();

                loop {
                    tick_timer.tick().await;
                    let mut buf: Vec<u8> = vec![0; 1024];
                    debug!("tick peer {:?}", peer);
                    match socket.read(&mut buf).await {
                        Ok(n) if n > 0 => {
                            info!("!!!!!!!!!!!! received something from peer {:?}", buf);
                        }
                        Err(e) => warn!("error reading peer loop {:?}", e),
                        // Ok(0) means that the stream has closed,
                        // the peer is not listening to us
                        Ok(0) => {
                            // warn!("peer closed connection");
                        }
                        _ => {}
                    };
                }
            });
        });
    }

    async fn add_magnet(m: Magnet, tx: Sender<TorrentMsg>) -> Result<(), Error> {
        info!("received add_magnet call");
        let info_hash = get_info_hash(&m.xt.unwrap());
        debug!("info_hash {:?}", info_hash);

        // first, do a `connect` handshake to the tracker
        let client = Tracker::connect(m.tr).await?;
        let peer_id = client.peer_id;

        // second, do a `announce` handshake to the tracker
        // and get the list of peers for this torrent
        let peers = client.announce_exchange(info_hash).await?;

        // listen to events on our peer socket,
        // that we used to announce to trackers.
        // peers will use this socket to send
        // handshakes
        let tx_client = tx.clone();
        spawn(async move {
            // 127
            client.run(tx_client.clone()).await;
        });

        info!("sending handshake req to {:?} peers...", peers.len());
        // Each peer will have its own event loop,
        // to listen to and send messages to `tracker`
        for peer in peers {
            debug!("trying to connect to {:?}", peer);

            let tx = tx.clone();

            spawn(async move {
                match timeout(Duration::from_secs(3), TcpStream::connect(peer)).await {
                    Ok(x) => match x {
                        Ok(mut socket) => {
                            let my_handshake = Handshake::new(info_hash, peer_id);
                            // try to send our handshake to the peer
                            if let Err(e) = timeout(
                                Duration::from_secs(3),
                                socket.write_all(&mut my_handshake.serialize().unwrap()),
                            )
                            .await
                            {
                                warn!("Failed to send handshake {e}");
                            } else {
                                debug!("sent handshake to {:?}", socket.peer_addr());

                                let mut buf: Vec<u8> = vec![];

                                // our handshake succeeded,
                                // now we try to receive one from the peer
                                debug!("waiting for response...");
                                let _ = timeout(Duration::new(5, 0), socket.read_to_end(&mut buf))
                                    .await
                                    .map_err(|_| Error::RequestTimeout)??;
                                // validate that the handshake from the peer
                                // is valid
                                let des = Handshake::deserialize(&buf)?;

                                info!("received handshake from peer {:?} {:?}", peer, des);
                                tx.send(TorrentMsg::ConnectedPeer(socket.peer_addr().unwrap()))
                                    .await
                                    .unwrap();

                                let mut interested = Interested::new().serialize()?;
                                socket.write_all(&mut interested).await?;
                            };
                        }
                        Err(_) => {
                            // debug!("peer connection refused, skipping");
                        }
                    },
                    Err(_) => {
                        // debug!("peer does download_dirnot support peer wire protocol, skipping");
                    }
                };
                Ok::<(), Error>(())
            });
        }
        Ok::<(), Error>(())
    }
}
