use crate::bitfield::Bitfield;
use crate::error::Error;
use crate::magnet_parser::get_info_hash;
use crate::peer::Peer;
use crate::tcp_wire::lib::BlockInfo;
use crate::tcp_wire::messages::Handshake;
use crate::tcp_wire::messages::Message;
use crate::tcp_wire::messages::PeerCodec;
use crate::tracker::tracker::Tracker;
use futures::stream::SplitSink;
use futures::{SinkExt, StreamExt};
use log::debug;
use log::info;
use magnet_url::Magnet;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio::select;
use tokio::spawn;
use tokio::sync::mpsc::Receiver;
use tokio::sync::mpsc::Sender;
use tokio::time::interval;
use tokio::time::Interval;
use tokio_util::codec::Framed;

#[derive(Debug)]
pub enum TorrentMsg {
    AddMagnet(Magnet),
    // Torrent will start with a blank bitfield
    // because it cannot know it from a magnet link
    // once a peer send the first bitfield message,
    // we will update the torrent bitfield
    UpdateBitfield(usize),
    Request(Peer, SplitSink<Framed<TcpStream, PeerCodec>, Message>),
}

#[derive(Debug)]
pub struct Torrent {
    pub bitfield: Bitfield,
    pub tx: Sender<TorrentMsg>,
    pub rx: Receiver<TorrentMsg>,
    pub tick_interval: Interval,
    pub in_end_game: bool,
    // todo: how to make this global to all peers?
    pub requested_pieces: Arc<Mutex<BTreeMap<u32, bool>>>,
}

impl Torrent {
    pub async fn new(tx: Sender<TorrentMsg>, rx: Receiver<TorrentMsg>) -> Self {
        let requested_pieces = Arc::new(Mutex::new(BTreeMap::default()));
        Self {
            bitfield: Bitfield::default(),
            in_end_game: false,
            tx,
            rx,
            requested_pieces,
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
                    TorrentMsg::Request(peer, sink) => {
                        //
                    }
                }
            }
        }
    }

    #[tracing::instrument]
    pub async fn listen_to_peer(
        tx: Sender<TorrentMsg>,
        mut peer: Peer,
        our_handshake: Handshake,
    ) -> Result<(), Error> {
        let mut tick_timer = interval(Duration::from_secs(1));
        let mut socket = TcpStream::connect(peer.addr).await?;

        // Send Handshake to peer
        socket.write_all(&mut our_handshake.serialize()?).await?;

        // Read Handshake from peer
        let mut handshake_buf = [0u8; 68];
        socket.read_exact(&mut handshake_buf).await?;
        let their_handshake = Handshake::deserialize(&handshake_buf)?;

        // Validate their handshake against ours
        if !their_handshake.validate(&our_handshake) {
            return Err(Error::HandshakeInvalid);
        }

        // Update peer_id that was received from
        // their handshake
        peer.id = Some(their_handshake.peer_id);

        let (mut sink, mut stream) = Framed::new(socket, PeerCodec).split();

        // If there is a Bitfield message to be received,
        // it will be the very first message after the handshake,
        // receive it here, add this information to peer
        // and create a new Bitfield for `Torrent` with the same length,
        // but completely empty
        let msg = stream.next().await;
        if let Some(Ok(Message::Bitfield(bitfield))) = msg {
            info!("Received Bitfield message from peer {:?}", peer);
            info!("{:?}", bitfield);
            // update the bitfield of the `Torrent`
            // will create a new, empty bitfield, with
            // the same len
            tx.send(TorrentMsg::UpdateBitfield(bitfield.inner.len()))
                .await
                .unwrap();
        }

        // Send Interested & Unchoke to peer
        // We want to send and receive blocks
        // from everyone
        sink.send(Message::Interested).await?;
        peer.am_interested = true;
        sink.send(Message::Unchoke).await?;
        peer.am_choking = false;

        loop {
            select! {
                _ = tick_timer.tick() => {
                    debug!("tick peer {:?}", peer);
                }
                Some(msg) = stream.next() => {
                    let msg = msg?;
                    match msg {
                        Message::KeepAlive => {
                            info!("Peer {:?} sent Keepalive", peer);
                        }
                        Message::Bitfield(bitfield) => {
                            info!("\t received bitfield");
                            info!("{:?}", bitfield);

                            let first = bitfield.into_iter().find(|x| x.bit == 1);

                            if let Some(first) = first {
                                info!("requesting first bit with index {:?}", first.index);
                                let block = BlockInfo::new().index(first.index as u32);
                                sink.send(Message::Request(block)).await?;
                                // todo: I have to wait until Unchoke to send
                                // most of the times, Bitfield and Have are immediately
                                // followed by an Unchoke,
                            }
                        }
                        Message::Unchoke => {
                            peer.peer_choking = false;
                            info!("Peer {:?} unchoked us", peer);

                            // the download flow (Request and Piece) msgs
                            // will start when the peer Unchokes us
                            // send the first request to peer here
                            // - logic fn piece.request
                            // loop
                            // 1 - get next piece from bitfield
                            // check:
                            // if it has already been requested,
                            // on the list: torrent.requested_pieces
                            // false? go back to 1
                            // true? update torrent.requested_pieces
                            // send the Request

                            if peer.am_interested {
                                // tx.send(TorrentMsg::Request(peer.clone(), &mut sink)).await;
                                // let block = BlockInfo::new().index(0);
                                // sink.send(Message::Request(block)).await?;
                            }
                        }
                        Message::Choke => {
                            peer.peer_choking = true;
                            info!("Peer {:?} choked us", peer);
                            // clear any pending requests
                        }
                        Message::Interested => {
                            peer.peer_interested = true;
                            info!("Peer {:?} is interested in us", peer);
                            // peer will start to request blocks from us soon
                        }
                        Message::NotInterested => {
                            peer.peer_interested = false;
                            info!("Peer {:?} is not interested in us", peer);
                            // peer won't request blocks from us anymore
                        }
                        Message::Have(piece_index) => {
                            debug!("Peer {:?} has a piece_index of {:?}", peer, piece_index);
                            // Have is usually sent when I peer has downloaded
                            // a new block, however, some peers, after handshake,
                            // send an incomplete bitfield followed by a sequence of
                            // have's. They do this to try to prevent censhorship
                            // from ISPs. This is one of the reasons why
                            // this client uses the Bitfield as a source of truth
                            // to manage pieces. For each Have message, we will
                            // overwrite the piece_index on the peer bitfield.

                            peer.pieces.set(piece_index);

                            let block = BlockInfo::new().index(piece_index as u32);
                            // this will be sent, as a request, on Unchoke
                            peer.pending_requests.push(block);
                        }
                        Message::Piece(block) => {
                            info!("Peer {:?} sent a Piece of index {}", peer, block.index);
                            info!("Block has {:?} KiB", block.block.len() / 1000);
                            // validate block,
                            // if we requested it,
                            // if it has 16KiB,
                            // if the hash is valid,
                            // false? remove from torrent.requested_blocks
                            // true? update our bitfield
                            // send msg to `Disk` tx
                            // Advertise to the peers that
                            // doesn't have this piece, that
                            // we Have it.
                            // Request another piece
                            // call fn piece.request from Unchoke logic
                            // ping pong of request & piece will start
                        }
                        Message::Cancel(block_info) => {
                            info!("Peer {:?} canceled a block {:?}", peer, block_info);
                        }
                        Message::Request(block_info) => {
                            info!("Peer {:?} request a block {:?}", peer, block_info);
                        }
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
        for peer in peers {
            let tx = self.tx.clone();
            let our_handshake = our_handshake.clone();

            debug!("listening to peer...");

            spawn(async move {
                Self::listen_to_peer(tx, peer, our_handshake).await?;
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
