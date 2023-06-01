use crate::error::Error;
use crate::magnet_parser::get_info_hash;
use crate::tcp_wire::messages::Handshake;
use crate::tcp_wire::messages::Interested;
use crate::tcp_wire::messages::Unchoke;
use crate::tracker::client::Client;
use log::debug;
use log::info;
use log::warn;
use magnet_url::Magnet;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::net::TcpStream;
use tokio::select;
use tokio::spawn;
use tokio::sync::mpsc::Receiver;
use tokio::sync::mpsc::Sender;
use tokio::time::timeout;

#[derive(Debug)]
pub enum BackendMessage {
    Quit,
    AddMagnet(Magnet),
    AddPeer(TcpStream),
}

#[derive(Debug)]
pub struct Backend {
    peers: Vec<TcpStream>,
    socket: TcpListener,
    tx: Sender<BackendMessage>,
    rx: Receiver<BackendMessage>,
}

impl Backend {
    pub async fn new(tx: Sender<BackendMessage>, rx: Receiver<BackendMessage>) -> Self {
        let peers = vec![];
        let socket = TcpListener::bind("0.0.0.0:6801").await.unwrap();

        Self {
            peers,
            tx,
            rx,
            socket,
        }
    }

    // pub fn handle(&self, )

    pub async fn run(&mut self) -> Result<(), Error> {
        let tick_rate = Duration::from_millis(200);

        // spawn(async move {
        loop {
            select! {
                Ok((mut socket, addr)) = self.socket.accept() => {
                    spawn(async move {
                        info!("!!! received connection on loop {:?}", addr);
                        let mut buf: Vec<u8> = Vec::with_capacity(1024);
                        loop {
                            match socket.read(&mut buf).await {
                                // Return value of `Ok(0)` signifies that the remote has
                                // closed
                                Ok(0) => return,
                                Ok(n) => {
                                    // Copy the data back to socket
                                    match socket.write_all(&buf[..n]).await {
                                        Ok(_) => {
                                            info!("Wrote {n} bytes to buf");
                                            info!("Buf is {:?}", buf);
                                        },
                                        Err(e) => {
                                            warn!("Failed to write to buf {e}");
                                            warn!("Stop processing...");
                                            return
                                        }
                                    }
                                }
                                Err(_) => {
                                    // Unexpected socket error. There isn't much we can do
                                    // here so just stop processing.
                                    return;
                                }
                            }
                        }
                    });
                },
                Some(msg) = self.rx.recv() => {
                    match msg {
                        BackendMessage::Quit => {
                            // todo
                        },
                        BackendMessage::AddMagnet(link) => {
                            let tx = self.tx.clone();
                            Backend::add_magnet(link, tx).await?;
                        },
                        BackendMessage::AddPeer(socket) => {
                            // this peer has been announced,
                            // it will send and ask pieces to this client
                            info!("pushing peer socket to vec {:?}", socket);
                            self.peers.push(socket);
                        }
                    }
                }
            }
        }
    }

    async fn add_magnet(m: Magnet, tx: Sender<BackendMessage>) -> Result<(), Error> {
        info!("received add_magnet call");
        let info_hash = get_info_hash(&m.xt.unwrap());
        debug!("info_hash {:?}", info_hash);

        // first, do a `connect` handshake to the tracker
        let client = Client::connect(m.tr).unwrap();

        // second, do a `announce` handshake to the tracker
        // and get the list of peers for this torrent
        let peers = client.announce_exchange(info_hash).unwrap();

        // for peer in vec!["181.171.140.208:51413"] {
        // spawn(async move {
        for peer in peers {
            info!("trying to connect to {:?}", peer);
            let handshake = Handshake::new(info_hash, client.peer_id);

            let mut socket = match timeout(Duration::from_secs(2), TcpStream::connect(peer)).await {
                Ok(x) => match x {
                    Ok(x) => x,
                    Err(_) => {
                        info!("peer connection refused, skipping");
                        continue;
                    }
                },
                Err(_) => {
                    debug!("peer does download_dirnot support peer wire protocol, skipping");
                    continue;
                }
            };

            info!("? socket {:#?}", socket);
            info!("* sending handshake...");

            // send our handshake to the peer
            socket
                .write_all(&mut handshake.serialize().unwrap())
                .await?;

            // receive a handshake back
            let mut buf = Vec::with_capacity(100);

            match timeout(Duration::from_secs(25), socket.read_to_end(&mut buf)).await {
                Ok(x) => {
                    let x = x.unwrap();

                    info!("* received handshake back");

                    if x == 0 {
                        continue;
                    };

                    info!("len {}", x);
                    info!("trying to deserialize it now");

                    let remote_handshake = Handshake::deserialize(&mut buf)?;

                    info!("{:?}", remote_handshake);

                    if remote_handshake.validate(handshake.clone()) {
                        info!("handshake is valid, sending Interested and Unchoke");

                        socket
                            .write_all(&mut Interested::new().serialize().unwrap())
                            .await?;

                        socket
                            .write_all(&mut Unchoke::new().serialize().unwrap())
                            .await?;

                        // connection with this peer is valid,
                        // add the socket to our peer vec.
                        // the event loop will have this
                        // new tcp socket and listen for messages
                        tx.send(BackendMessage::AddPeer(socket)).await.unwrap();
                    }
                }
                Err(_) => {
                    warn!("connection timeout");
                }
            }
        }
        Ok::<(), Error>(())
    }
}
