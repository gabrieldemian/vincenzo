use crate::error::Error;
use crate::magnet_parser::get_info_hash;
use crate::tcp_wire::messages::Handshake;
use crate::tcp_wire::messages::Interested;
use crate::tracker::tracker::Tracker;
use log::debug;
use log::info;
use log::warn;
use magnet_url::Magnet;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::select;
use tokio::spawn;
use tokio::sync::mpsc::Receiver;
use tokio::sync::mpsc::Sender;
use tokio::time::interval;
use tokio::time::timeout;

#[derive(Debug)]
pub enum TorrentMsg {
    Quit,
    AddMagnet(Magnet),
    AddPeer(TcpStream),
}

#[derive(Debug)]
pub struct Torrent {
    peers: Vec<TcpStream>,
    // socket: TcpListener,
    tx: Sender<TorrentMsg>,
    rx: Receiver<TorrentMsg>,
}

impl Torrent {
    pub async fn new(tx: Sender<TorrentMsg>, rx: Receiver<TorrentMsg>) -> Self {
        let peers = vec![];
        // let socket = TcpListener::bind("0.0.0.0:0").await.unwrap();

        Self {
            peers,
            tx,
            rx,
            // socket,
        }
    }

    pub async fn run(&mut self) -> Result<(), Error> {
        let mut tick_timer = interval(Duration::from_secs(1));

        loop {
            select! {
                _tick = tick_timer.tick() => {
                    println!("tick torrent");
                }
                Some(msg) = self.rx.recv() => {
                    match msg {
                        TorrentMsg::Quit => {
                            // todo
                        },
                        TorrentMsg::AddMagnet(link) => {
                            let tx = self.tx.clone();
                            Torrent::add_magnet(link, tx).await?;
                        },
                        TorrentMsg::AddPeer(socket) => {
                            // this peer has been handshake'd
                            // and is ready to send/receive msgs
                            info!("listening to msgs from {:?}", socket.peer_addr());
                            spawn(async move {
                                // let mut interested = Interested::new().serialize().unwrap();
                                // socket.write_all(&mut interested).await.unwrap();
                                loop {
                                    socket.readable().await.unwrap();
                                    let mut buf: Vec<u8> = vec![];
                                    if let Ok(n) = socket.try_read(&mut buf) {
                                        if n > 0 {
                                            info!("received buf from {:?}", socket.peer_addr());
                                            info!("buf is {:?}", buf);
                                        }
                                    }
                                }
                            });
                        }
                    }
                }
            }
        }
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

        // let tx_client = tx.clone();
        // spawn(async move {
        //     client.run(tx_client.clone()).await;
        // });

        // Each peer will have its own event loop,
        // to listen to and send messages to `tracker`
        for peer in peers {
            debug!("trying to connect to {:?}", peer);

            let tx = tx.clone();

            spawn(async move {
                match timeout(Duration::from_secs(3), TcpStream::connect(peer)).await {
                    Ok(x) => match x {
                        Ok(mut socket) => {
                            info!("* connected, sending handshake...");
                            let handshake = Handshake::new(info_hash, peer_id);
                            // try to send our handshake to the peer
                            if let Err(e) =
                                socket.write_all(&mut handshake.serialize().unwrap()).await
                            {
                                warn!("Failed to send handshake {e}");
                            } else {
                                info!("sent handshake to {:?}", socket.peer_addr());

                                let mut buf: Vec<u8> = vec![];

                                // our handshake succeeded,
                                // now we try to receive one from the peer
                                if let Ok(_) = socket.read_to_end(&mut buf).await {
                                    let des = Handshake::deserialize(&buf);

                                    if let Ok(des) = des {
                                        info!("deserialized handshake from peer {:?}", des);
                                        tx.send(TorrentMsg::AddPeer(socket)).await.unwrap();
                                    }
                                }
                            };
                        }
                        Err(_) => {
                            debug!("peer connection refused, skipping");
                        }
                    },
                    Err(_) => {
                        debug!("peer does download_dirnot support peer wire protocol, skipping");
                    }
                };
            });
        }
        Ok::<(), Error>(())
    }
}
