use futures::{SinkExt, StreamExt};
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::Arc,
    time::Duration,
};
use tokio::io::AsyncWriteExt;
use tokio::{io::AsyncReadExt, select, sync::mpsc::Sender};
use tokio_util::codec::Framed;

use log::{debug, info};
use tokio::{net::TcpStream, time::interval};

use crate::{
    bitfield::Bitfield,
    error::Error,
    tcp_wire::{
        lib::BlockInfo,
        messages::{Handshake, Message, PeerCodec},
    },
    torrent::{Torrent, TorrentCtx, TorrentMsg},
};

#[derive(Debug, Clone)]
pub struct Peer {
    /// if this client is choking this peer
    pub am_choking: bool,
    /// if this client is interested in this peer
    pub am_interested: bool,
    /// if this peer is choking the client
    pub peer_choking: bool,
    /// if this peer is interested in the client
    pub peer_interested: bool,
    /// a `Bitfield` with pieces that this peer
    /// has, and hasn't, containing 0s and 1s
    pub pieces: Bitfield,
    /// TCP addr that this peer is listening on
    pub addr: SocketAddr,
    pub id: Option<[u8; 20]>,
    /// Requests that we'll send to this peer,
    /// once he unchoke us
    pub pending_requests: Vec<BlockInfo>,
    pub torrent: Option<Arc<Torrent>>,
}

impl Default for Peer {
    fn default() -> Self {
        Self {
            // connections start out choked and uninterested,
            // from both sides
            am_choking: true,
            am_interested: false,
            peer_choking: true,
            peer_interested: false,
            pieces: Bitfield::default(),
            addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0),
            pending_requests: vec![],
            id: None,
            torrent: None,
        }
    }
}

// create a Peer from a `SocketAddr`. Used after
// an announce request with a tracker
impl From<SocketAddr> for Peer {
    fn from(addr: SocketAddr) -> Self {
        let mut s: Self = Self::default();
        s.addr = addr;
        s
    }
}

impl Peer {
    pub fn new(torrent: Arc<Torrent>) -> Self {
        let mut p = Self::default();
        p.torrent = Some(torrent);
        p
    }
    pub async fn run(
        &mut self,
        our_handshake: Handshake,
        ctx: Arc<TorrentCtx>,
        tx: Sender<TorrentMsg>,
    ) -> Result<(), Error> {
        let mut tick_timer = interval(Duration::from_secs(1));
        let mut socket = TcpStream::connect(self.addr).await?;

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
        self.id = Some(their_handshake.peer_id);

        let (mut sink, mut stream) = Framed::new(socket, PeerCodec).split();

        // If there is a Bitfield message to be received,
        // it will be the very first message after the handshake,
        // receive it here, add this information to peer
        // and create a new Bitfield for `Torrent` with the same length,
        // but completely empty
        let msg = stream.next().await;
        if let Some(Ok(Message::Bitfield(bitfield))) = msg {
            info!("Received Bitfield message from peer {:?}", self.addr);
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
        self.am_interested = true;
        sink.send(Message::Unchoke).await?;
        self.am_choking = false;

        loop {
            select! {
                _ = tick_timer.tick() => {
                    debug!("tick peer {:?}", self.addr);
                }
                Some(msg) = stream.next() => {
                    let msg = msg?;
                    match msg {
                        Message::KeepAlive => {
                            info!("Peer {:?} sent Keepalive", self.addr);
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
                            self.peer_choking = false;
                            info!("Peer {:?} unchoked us", self.addr);

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

                            if self.am_interested {
                                let rp = ctx.requested_pieces.read().await;
                                let piece = self.pieces.clone().zip(rp.iter()).find(|(p, r)| p.index != **r as usize);

                                if let Some(piece) = piece {
                                    println!("found a piece {:?}", piece.0);
                                }
                            }
                        }
                        Message::Choke => {
                            self.peer_choking = true;
                            self.pending_requests.clear();
                            info!("Peer {:?} choked us", self);
                            // clear any pending requests
                        }
                        Message::Interested => {
                            self.peer_interested = true;
                            info!("Peer {:?} is interested in us", self.addr);
                            // peer will start to request blocks from us soon
                        }
                        Message::NotInterested => {
                            self.peer_interested = false;
                            info!("Peer {:?} is not interested in us", self.addr);
                            // peer won't request blocks from us anymore
                        }
                        Message::Have(piece) => {
                            debug!("Peer {:?} has a piece_index of {:?}", self.addr, piece);
                            // Have is usually sent when I peer has downloaded
                            // a new block, however, some peers, after handshake,
                            // send an incomplete bitfield followed by a sequence of
                            // have's. They do this to try to prevent censhorship
                            // from ISPs. This is one of the reasons why
                            // this client uses the Bitfield as a source of truth
                            // to manage pieces. For each Have message, we will
                            // overwrite the piece_index on the peer bitfield.
                            self.pieces.set(piece as usize);
                            let block = BlockInfo::new().index(piece as u32);
                            // this will be sent, as a request, on Unchoke
                            self.pending_requests.push(block);
                        }
                        Message::Piece(block) => {
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
                            info!("Peer {:?} canceled a block {:?}", self.addr, block_info);
                        }
                        Message::Request(block_info) => {
                            info!("Peer {:?} request a block {:?}", self.addr, block_info);
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a() {
        //
    }
}
