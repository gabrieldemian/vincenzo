use futures::{SinkExt, StreamExt};
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::Arc,
    time::Duration,
};
use tokio::{io::AsyncReadExt, select, sync::mpsc::Sender};
use tokio::{io::AsyncWriteExt, time::timeout};
use tokio_util::codec::Framed;

use log::{debug, info};
use tokio::{net::TcpStream, time::interval};

use crate::{
    bitfield::Bitfield,
    error::Error,
    magnet_parser::get_info_hash,
    tcp_wire::{
        lib::BlockInfo,
        messages::{Handshake, Message, PeerCodec},
    },
    torrent::{TorrentCtx, TorrentMsg},
    tracker::tracker::TrackerCtx,
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
    /// Updated when the peer sends us its peer
    /// id, in the handshake.
    pub id: Option<[u8; 20]>,
    /// Requests that we'll send to this peer,
    /// once he unchoke us
    pub pending_pieces: Vec<u32>,
    pub torrent_ctx: Option<Arc<TorrentCtx>>,
    pub tracker_ctx: Arc<TrackerCtx>,
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
            pending_pieces: vec![],
            id: None,
            torrent_ctx: None,
            tracker_ctx: Arc::new(TrackerCtx::default()),
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
    pub fn new(torrent_ctx: Arc<TorrentCtx>) -> Self {
        let mut p = Self::default();
        p.torrent_ctx = Some(torrent_ctx);
        p
    }
    pub fn addr(mut self, addr: SocketAddr) -> Self {
        self.addr = addr;
        self
    }
    pub fn torrent_ctx(mut self, ctx: Arc<TorrentCtx>) -> Self {
        self.torrent_ctx = Some(ctx);
        self
    }
    pub fn id(mut self, id: [u8; 20]) -> Self {
        self.id = Some(id);
        self
    }
    pub async fn run(
        &mut self,
        tx: Sender<TorrentMsg>,
        tcp_stream: Option<TcpStream>,
    ) -> Result<(), Error> {
        let mut tick_timer = interval(Duration::from_secs(1));
        let mut socket = tcp_stream.unwrap_or(TcpStream::connect(self.addr).await?);

        let torrent_ctx = self.torrent_ctx.clone().unwrap();
        let tracker_ctx = self.tracker_ctx.clone();
        let xt = torrent_ctx.magnet.xt.as_ref().unwrap();

        let info_hash = get_info_hash(xt);
        let our_handshake = Handshake::new(info_hash, tracker_ctx.peer_id);

        // Send Handshake to peer
        socket.write_all(&mut our_handshake.serialize()?).await?;

        // Read Handshake from peer
        let mut handshake_buf = [0u8; 68];
        socket
            .read_exact(&mut handshake_buf)
            .await
            .expect("read handshake_buf");

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
        let msg = timeout(Duration::new(2, 0), stream.next()).await;
        // let msg = stream.next().await;

        if let Ok(msg) = msg {
            if let Some(Ok(Message::Bitfield(bitfield))) = msg {
                info!("Received Bitfield message from peer {:?}", self.addr);

                // update the bitfield of the `Torrent`
                // will create a new, empty bitfield, with
                // the same len
                tx.send(TorrentMsg::UpdateBitfield(bitfield.len_bytes()))
                    .await
                    .unwrap();

                bitfield.clone().for_each(|p| {
                    self.pending_pieces.push(p.index as u32);
                });
                self.pieces = bitfield;

                // debug!("pieces bitfield {:?}", self.pieces);
                // info!("pending {:?}", self.pending_pieces);
            }
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
                            info!("\t received late bitfield");
                            // take entire pieces from bitfield
                            // and put in pending_requests
                            bitfield.clone().for_each(|p| {
                                self.pending_pieces.push(p.index as u32);
                            });
                            self.pieces = bitfield;
                            // debug!("pieces {:?}", self.pieces);
                            // info!("pending {:?}", self.pending_pieces);
                        }
                        Message::Unchoke => {
                            self.peer_choking = false;
                            info!("Peer {:?} unchoked us", self.addr);

                            // the download flow (Request and Piece) msgs
                            // will start when the peer Unchokes us
                            // send the first request to peer here
                            // when he answers us,
                            // on the corresponding Piece message,
                            // send another Request

                            if self.am_interested {

                                let piece = self.pending_pieces.pop();
                                if let Some(piece) = piece {
                                    info!("available piece {piece}, requesting...");

                                    let block = BlockInfo::new().index(piece);
                                    let mut rp = torrent_ctx.requested_pieces.write().await;

                                    // was_requested is failing because
                                    // this set is not working because the bitfield
                                    // does not have the length of the index
                                    rp.set(block.index as usize);
                                    sink.send(Message::Request(block)).await.unwrap();
                                }

                                // let rp = torrent_ctx.requested_pieces.read().await;
                                // info!("torrent.requested_pieces {:?}", rp);
                                // let piece =
                                //     rp.clone()
                                //     .zip(self.pieces.clone())
                                //     .find(|(tp, p)| p.bit == 1 as u8 && tp.bit == 0);
                                //
                                // if let Some(piece) = piece {
                                //     info!("--- found valid, piece, requesting {:?}", piece.1.index);
                                //     let block = BlockInfo::new().index(piece.1.index as u32);
                                //     sink.send(Message::Request(block)).await.unwrap();
                                //
                                //     let mut rp = torrent_ctx.requested_pieces.write().await;
                                //     rp.set(piece.1.index);
                                // }
                            }

                            // if self.am_interested {
                            //     let rp = torrent_ctx.requested_pieces.read().await;
                            //
                            //     // find the next piece of the peer pieces,
                            //     // which has not been requested yet
                            //     let piece = self.pieces
                            //         .clone()
                            //         .zip(rp.clone())
                            //         .find(|(bt, tr)| bt.bit == 1 as u8 && tr.bit == 0);
                            //
                            //     if let Some(piece) = piece {
                            //         let block = BlockInfo::new().index(piece.0.index as u32);
                            //         sink.send(Message::Request(block)).await.unwrap();
                            //
                            //         let mut rp = torrent_ctx.requested_pieces.write().await;
                            //         rp.set(piece.0.index);
                            //         // todo: remove piece from peer.pending_requests
                            //     }
                            // }
                        }
                        Message::Choke => {
                            info!("Peer {:?} choked us", self.addr);
                            self.peer_choking = true;
                            self.pending_pieces.clear();
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
                            // from ISPs.
                            // Overwrite pieces on bitfield, if the peer has one
                            // info!("pieces {:?}", self.pieces);
                            self.pieces.set(piece as usize);
                            self.pending_pieces.push(piece as u32);
                            // info!("pending {:?}", self.pending_pieces);
                        }
                        Message::Piece(block) => {
                            info!("!!! received a block");
                            info!("index: {:?}", block.index);
                            info!("begin: {:?}", block.begin);
                            info!("block size: {:?} KiB", block.block.len() / 1000);
                            info!("is valid: {:?}", block.is_valid());

                            let rp = torrent_ctx.requested_pieces.read().await;
                            info!("rp? {:?}", rp);
                            let was_requested = rp.has(block.index);
                            info!("was_requested? {:?}", was_requested);
                            // let was_requested = self.pending_pieces.iter().find(|b| **b == block.index as u32);

                            if was_requested.is_some() && block.is_valid() {
                                info!("valid block\n");
                                // send msg to `Disk` tx
                                // Advertise to the peers that
                                // doesn't have this piece, that
                                // we Have it.
                                // Request another piece
                                // call fn piece.request from Unchoke logic
                                // ping pong of request & piece will start
                            } else {
                                // block not valid, remove it from requested blocks
                                // let mut rp = torrent_ctx.requested_pieces.write().await;
                                // rp.clear(block.index);
                            }
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
    use crate::{
        bitfield::BitItem,
        tcp_wire::lib::{Block, BLOCK_LEN},
    };

    use super::*;

    #[test]
    fn validate_block() {
        // requested pieces from torrent
        let mut rp = Bitfield::from(vec![0b1000_0000]);
        // block received from Piece
        let block = Block {
            index: 0,
            block: vec![0u8; BLOCK_LEN as usize],
            begin: 0,
        };
        let was_requested = rp.find(|b| b.index == block.index);

        // check if we requested this block
        assert!(was_requested.is_some());

        if was_requested.is_some() {
            // check if the block is 16 <= KiB
            assert!(block.is_valid());
        }
    }

    #[test]
    fn piece_picker_algo() {
        // bitfield of the torrent, representing the pieces
        // that we have downloaded (if 1)
        let torrent_bitfield = Bitfield::from(vec![0b0000_0000]);

        // bitfield from Bitfield message
        let bitfield = Bitfield::from(vec![0b1111_1111]);

        // look for a piece in `bitfield` that has not
        // been request on `torrent_bitfield`
        let piece = bitfield
            .clone()
            .zip(torrent_bitfield)
            .find(|(bt, tr)| bt.bit == 1 as u8 && tr.bit == 0);

        if let Some((piece, _)) = piece {
            assert_eq!(piece, BitItem { index: 0, bit: 1 })
        }

        // ------------

        let torrent_bitfield = Bitfield::from(vec![0b1000_0000]);
        let bitfield = Bitfield::from(vec![0b1111_1111]);
        let piece = bitfield
            .clone()
            .zip(torrent_bitfield)
            .find(|(bt, tr)| bt.bit == 1 as u8 && tr.bit == 0);

        if let Some((piece, _)) = piece {
            assert_eq!(piece, BitItem { index: 1, bit: 1 })
        }

        // ------------

        let torrent_bitfield = Bitfield::from(vec![0b1100_0000]);
        let bitfield = Bitfield::from(vec![0b0111_1111]);
        let piece = bitfield
            .clone()
            .zip(torrent_bitfield)
            .find(|(bt, tr)| bt.bit == 1 as u8 && tr.bit == 0);

        if let Some((piece, _)) = piece {
            assert_eq!(piece, BitItem { index: 2, bit: 1 })
        }

        // ------------

        let torrent_bitfield = Bitfield::from(vec![0b1111_1110]);
        let bitfield = Bitfield::from(vec![0b1111_1111]);
        let piece = bitfield
            .clone()
            .zip(torrent_bitfield)
            .find(|(bt, tr)| bt.bit == 1 as u8 && tr.bit == 0);

        if let Some((piece, _)) = piece {
            assert_eq!(piece, BitItem { index: 7, bit: 1 })
        }

        // ------------

        let torrent_bitfield = Bitfield::from(vec![0b1111_1111]);
        let bitfield = Bitfield::from(vec![0b1111_1111]);
        let piece = bitfield
            .clone()
            .zip(torrent_bitfield)
            .find(|(bt, tr)| bt.bit == 1 as u8 && tr.bit == 0);

        assert_eq!(piece, None)
    }
}
