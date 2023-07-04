use bendy::decoding::FromBencode;
use bitlab::SingleBits;
use futures::{stream::SplitSink, SinkExt, StreamExt};
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::Arc,
    time::Duration,
};
use tokio::io::AsyncWriteExt;
use tokio::{
    io::AsyncReadExt,
    select,
    sync::{mpsc::Sender, oneshot},
    time::Instant,
};
use tokio_util::codec::Framed;

use tokio::{net::TcpStream, time::interval};
use tracing::{debug, info, warn};

use crate::{
    bitfield::Bitfield,
    disk::DiskMsg,
    error::Error,
    extension::{Extension, Metadata},
    magnet_parser::get_info_hash,
    tcp_wire::{
        lib::Block,
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
    // pub requested_pieces: Bitfield,
    /// TCP addr that this peer is listening on
    pub addr: SocketAddr,
    pub extension: Extension,
    /// Updated when the peer sends us its peer
    /// id, in the handshake.
    pub id: Option<[u8; 20]>,
    pub torrent_ctx: Option<Arc<TorrentCtx>>,
    pub tracker_ctx: Arc<TrackerCtx>,
    pub disk_tx: Option<Sender<DiskMsg>>,
}

impl Default for Peer {
    fn default() -> Self {
        Self {
            am_choking: true,
            am_interested: false,
            extension: Extension::default(),
            peer_choking: true,
            peer_interested: false,
            pieces: Bitfield::default(),
            addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0),
            id: None,
            torrent_ctx: None,
            disk_tx: None,
            tracker_ctx: Arc::new(TrackerCtx::default()),
        }
    }
}

// create a Peer from a `SocketAddr`. Used after
// an announce request with a tracker
impl From<SocketAddr> for Peer {
    fn from(addr: SocketAddr) -> Self {
        Peer {
            addr,
            ..Default::default()
        }
    }
}

impl Peer {
    pub fn new(torrent_ctx: Arc<TorrentCtx>) -> Self {
        Peer {
            torrent_ctx: Some(torrent_ctx),
            ..Default::default()
        }
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
    /// Request a new piece that hasn't been requested before,
    /// the logic to pick a new piece is simple:
    /// `find` the next piece from this peer, that doesn't exist
    /// on the pieces of `Torrent`. The result is that the pieces will
    /// be requested sequentially.
    /// Ideally, it should request the rarest pieces first,
    /// but this is good enough for the initial version.
    #[tracing::instrument(skip(self, sink))]
    pub async fn request_next_piece(
        &mut self,
        sink: &mut SplitSink<Framed<TcpStream, PeerCodec>, Message>,
    ) -> Result<(), Error> {
        let torrent_ctx = self.torrent_ctx.clone().unwrap();
        let tr_pieces = torrent_ctx.pieces.read().await;

        // disk cannot be None at this point, this is safe
        let disk_tx = self.disk_tx.clone().unwrap();

        info!("downloaded {tr_pieces:?}");
        info!("tr_pieces len: {:?}", tr_pieces.len());
        info!("self.pieces: {:?}", self.pieces);
        info!("self.pieces len: {:?}", self.pieces.len());

        // get a list of unique block_infos from the Disk,
        // those are already marked as requested on Torrent
        let (otx, orx) = oneshot::channel();
        let _ = disk_tx
            .send(DiskMsg::RequestBlocks((
                // self.extension.reqq.unwrap_or(10) as usize,
                5, otx,
            )))
            .await;

        let r = orx.await.unwrap();

        for info in r {
            sink.send(Message::Request(info)).await?;
        }

        Ok(())
    }
    #[tracing::instrument(skip(self, tx, tcp_stream), name = "peer::run")]
    pub async fn run(
        &mut self,
        tx: Sender<TorrentMsg>,
        tcp_stream: Option<TcpStream>,
    ) -> Result<(), Error> {
        let mut socket = tcp_stream.unwrap_or(TcpStream::connect(self.addr).await?);

        let torrent_ctx = self.torrent_ctx.clone().unwrap();
        let tracker_ctx = self.tracker_ctx.clone();
        let xt = torrent_ctx.magnet.xt.as_ref().unwrap();

        let info_hash = get_info_hash(xt);
        let our_handshake = Handshake::new(info_hash, tracker_ctx.peer_id);

        // Send Handshake to peer
        socket.write_all(&our_handshake.serialize()?).await?;

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

        // check if peer supports Extended Protocol
        if let Ok(true) = their_handshake.reserved[5].get_bit(3) {
            // extended handshake must be sent after the handshake
            sink.send(Message::Extended((0, vec![]))).await?;
        }

        // Send Interested & Unchoke to peer
        // We want to send and receive blocks
        // from everyone
        sink.send(Message::Interested).await?;
        self.am_interested = true;
        sink.send(Message::Unchoke).await?;
        self.am_choking = false;

        let mut keep_alive_timer = interval(Duration::from_secs(1));

        let mut request_timer = interval(Duration::from_secs(5));

        let keepalive_interval = Duration::from_secs(120);

        let mut last_tick_keepalive = Instant::now();

        loop {
            select! {
                _ = keep_alive_timer.tick() => {
                    // send Keepalive every 2 minutes
                    if last_tick_keepalive.elapsed() >= keepalive_interval {
                        last_tick_keepalive = Instant::now();
                        sink.send(Message::KeepAlive).await?;
                    }
                }
                // Sometimes that blocks requested are never sent to us,
                // this can happen for a lot of reasons, and is normal and expected.
                // The algorithm to re-request those blocks is simple:
                // At every 5 seconds, check for blocks that were requested,
                // but not downloaded, and request them again.
                _ = request_timer.tick() => {
                    // check if there is requested blocks that hasnt been downloaded
                    let downloaded = torrent_ctx.downloaded_blocks.read().await;
                    let requested = torrent_ctx.requested_blocks.read().await;

                    let blocks_len = torrent_ctx.info.read().await.blocks_len();

                    if (downloaded.len() as u32) < (blocks_len) {
                        for req in &*requested {
                            let f = downloaded.iter().find(|down| **down == *req);
                            if let None = f {
                                let _ = sink.send(Message::Request(req.clone())).await;
                            }
                        }
                    }

                }
                Some(msg) = stream.next() => {
                    let msg = msg?;
                    match msg {
                        Message::KeepAlive => {
                            info!("--------------------------------");
                            info!("| {:?} Keepalive  |", self.addr);
                            info!("--------------------------------");
                        }
                        Message::Bitfield(bitfield) => {
                            // take entire pieces from bitfield
                            // and put in pending_requests
                            info!("----------------------------------");
                            info!("| {:?} Bitfield  |", self.addr);
                            info!("----------------------------------\n");
                            self.pieces = bitfield.clone();

                            // update the bitfield of the `Torrent`
                            // will create a new, empty bitfield, with
                            // the same len
                            tx.send(TorrentMsg::UpdateBitfield(bitfield.len_bytes()))
                                .await
                                .unwrap();

                            info!("{:?}", self.pieces);
                            info!("------------------------------\n");
                        }
                        Message::Unchoke => {
                            self.peer_choking = false;
                            info!("---------------------------------");
                            info!("| {:?} Unchoke  |", self.addr);
                            info!("---------------------------------");

                            // the download flow (Request and Piece) msgs
                            // will start when the peer Unchokes us
                            // send the first request to peer here
                            // when he answers us,
                            // on the corresponding Piece message,
                            // send another Request
                            if self.am_interested {
                                self.request_next_piece(&mut sink).await?;
                            }
                            info!("---------------------------------\n");
                        }
                        Message::Choke => {
                            self.peer_choking = true;
                            info!("--------------------------------");
                            info!("| {:?} Choke  |", self.addr);
                            info!("---------------------------------");
                        }
                        Message::Interested => {
                            info!("------------------------------");
                            info!("| {:?} Interested  |", self.addr);
                            info!("-------------------------------");
                            self.peer_interested = true;
                            // peer will start to request blocks from us soon
                        }
                        Message::NotInterested => {
                            info!("------------------------------");
                            info!("| {:?} NotInterested  |", self.addr);
                            info!("-------------------------------");
                            self.peer_interested = false;
                            // peer won't request blocks from us anymore
                        }
                        Message::Have(piece) => {
                            debug!("-------------------------------");
                            debug!("| {:?} Have  |", self.addr);
                            debug!("-------------------------------");
                            // Have is usually sent when the peer has downloaded
                            // a new piece, however, some peers, after handshake,
                            // send an incomplete bitfield followed by a sequence of
                            // have's. They do this to try to prevent censhorship
                            // from ISPs.
                            // Overwrite pieces on bitfield, if the peer has one
                            // info!("pieces {:?}", self.pieces);
                            self.pieces.set(piece);
                        }
                        Message::Piece(block) => {
                            info!("-------------------------------");
                            info!("| {:?} Piece  |", self.addr);
                            info!("-------------------------------");
                            info!("index: {:?}", block.index);
                            info!("begin: {:?}", block.begin);
                            info!("block size: {:?} bytes", block.block.len());
                            info!("block size: {:?} KiB", block.block.len() / 1000);
                            info!("is valid? {:?}", block.is_valid());

                            if block.is_valid() {
                                let info = torrent_ctx.info.read().await;

                                if block.begin + block.block.len() as u32 >= info.piece_length {
                                    let (tx, rx) = oneshot::channel();
                                    let disk_tx = self.disk_tx.as_ref().unwrap();

                                    // Ask Disk to validate the bytes of all blocks of this piece
                                    let _ = disk_tx.send(DiskMsg::ValidatePiece((block.index, tx))).await;
                                    let r = rx.await;

                                    // Hash of piece is valid
                                    if let Ok(Ok(r)) = r {
                                        let mut tr_pieces = torrent_ctx.pieces.write().await;
                                        tr_pieces.set(block.index);
                                        // send Have msg to peers that dont have this piece
                                    } else {
                                        warn!("The hash of the piece {:?} is invalid", block.index);
                                    }
                                }

                                let (tx, rx) = oneshot::channel();
                                let disk_tx = self.disk_tx.as_ref().unwrap();

                                disk_tx.send(DiskMsg::WriteBlock((block.clone(), tx))).await.unwrap();

                                let r = rx.await.unwrap();

                                match r {
                                    Ok(_) => {
                                        let mut bd = torrent_ctx.downloaded_blocks.write().await;
                                        let was_downloaded = bd.iter().any(|b| *b == block.clone().into() );

                                        if !was_downloaded {
                                            bd.push_front(block.into());
                                        }
                                        drop(bd);

                                        info!("wrote piece with success on disk");
                                    }
                                    Err(e) => warn!("could not write piece to disk {e:#?}")
                                }

                                // todo: advertise to peers that
                                // we Have this piece, if this is the last block of a piece
                                // and the piece has the right Hash on Info
                            } else {
                                // block not valid nor requested,
                                // remove it from requested blocks
                                warn!("invalid block from Piece");
                                let mut tr_pieces = torrent_ctx.pieces.write().await;
                                tr_pieces.clear(block.index);
                                warn!("deleted, new tr_pieces {:?}", *tr_pieces);
                            }

                            info!("---------------------------------\n");

                            self.request_next_piece(&mut sink).await?;
                        }
                        Message::Cancel(block_info) => {
                            info!("------------------------------");
                            info!("| {:?} cancel  |", self.addr);
                            info!("------------------------------");
                            info!("{block_info:?}");
                        }
                        Message::Request(block_info) => {
                            info!("------------------------------");
                            info!("| {:?} Request  |", self.addr);
                            info!("------------------------------");
                            info!("{block_info:?}");

                            let disk_tx = self.disk_tx.clone().unwrap();
                            let (tx, rx) = oneshot::channel();

                            let _ = disk_tx.send(DiskMsg::ReadBlock((block_info.clone(), tx))).await;

                            let r = rx.await;

                            if let Ok(Ok(bytes)) = r {
                                let block = Block {
                                    index: block_info.index as usize,
                                    begin: block_info.begin,
                                    block: bytes,
                                };
                                let _ = sink.send(Message::Piece(block)).await;
                            }
                        }
                        Message::Extended((ext_id, payload)) => {
                            info!("----------------------------------");
                            info!("| {:?} Extended  |", self.addr);
                            info!("----------------------------------");

                            // ext_id of 0 means a bep10 handshake
                            if ext_id == 0 {
                                if self.extension.m.ut_metadata.is_none() {
                                    let extension = Extension::from_bencode(&payload);

                                    if let Ok(ext) = extension {
                                        // info!("extension {ext:#?}");
                                        // println!("received ext handshake {ext:#?}");
                                        self.extension = ext;

                                        // send bep09 request to get the Info
                                        if let Some(ut_metadata) = self.extension.m.ut_metadata {
                                            info!("peer supports ut_metadata {ut_metadata}, sending request");

                                            let h = b"d8:msg_typei0e5:piecei0ee";

                                            info!("sending request msg with ut_metadata {ut_metadata:?}");
                                            sink.send(Message::Extended((ut_metadata, h.to_vec()))).await?;
                                        }

                                    }
                                } else if payload.len() >= 12 {
                                    // can be a peer sending the handshake,
                                    // or the response of a request (a data)
                                    let pair = &payload[12..=13];
                                    let info_begin = payload.len() - self.extension.metadata_size.unwrap() as usize;

                                    if pair == b"1e" {
                                        if let Ok((_, info)) = Metadata::extract(payload, info_begin as usize) {
                                            let _ = self.disk_tx.as_ref().unwrap().send(DiskMsg::NewTorrent(info)).await;
                                            if self.am_interested && !self.peer_choking {
                                                self.request_next_piece(&mut sink).await?;
                                            }
                                        }
                                    }
                                }
                            }
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
    fn can_get_requested_blocks_but_not_downloaded() {
        let requested = vec![0, 1, 2, 3, 4, 5, 6];
        let downloaded = vec![0, 1, 2, 3];
        let mut missing: Vec<i32> = Vec::new();

        for req in &*requested {
            let f = downloaded.iter().find(|down| **down == *req);
            if let None = f {
                missing.push(*req);
            }
        }

        assert_eq!(missing, vec![4, 5, 6]);
    }

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
        let torrent_bitfield = Bitfield::from(vec![0u8; 32]);

        let bitfield = Bitfield::from(vec![0u8; 32]);

        let piece = bitfield
            .zip(torrent_bitfield)
            .find(|(bt, tr)| bt.bit == 1_u8 && tr.bit == 0);

        // ------------

        // bitfield of the torrent, representing the pieces
        // that we have downloaded so far (if 1)
        let torrent_bitfield = Bitfield::from(vec![0b0000_0000]);

        // bitfield from Bitfield message
        let bitfield = Bitfield::from(vec![0b1111_1111]);

        // look for a piece in `bitfield` that has not
        // been request on `torrent_bitfield`
        let piece = bitfield
            .zip(torrent_bitfield)
            .find(|(bt, tr)| bt.bit == 1_u8 && tr.bit == 0);

        if let Some((piece, _)) = piece {
            assert_eq!(piece, BitItem { index: 0, bit: 1 })
        }

        // ------------

        let torrent_bitfield = Bitfield::from(vec![0b1000_0000]);
        let bitfield = Bitfield::from(vec![0b1111_1111]);
        let piece = bitfield
            .zip(torrent_bitfield)
            .find(|(bt, tr)| bt.bit == 1_u8 && tr.bit == 0);

        if let Some((piece, _)) = piece {
            assert_eq!(piece, BitItem { index: 1, bit: 1 })
        }

        // ------------

        let torrent_bitfield = Bitfield::from(vec![0b1100_0000]);
        let bitfield = Bitfield::from(vec![0b0111_1111]);
        let piece = bitfield
            .zip(torrent_bitfield)
            .find(|(bt, tr)| bt.bit == 1_u8 && tr.bit == 0);

        if let Some((piece, _)) = piece {
            assert_eq!(piece, BitItem { index: 2, bit: 1 })
        }

        // ------------

        let torrent_bitfield = Bitfield::from(vec![0b1111_1110]);
        let bitfield = Bitfield::from(vec![0b1111_1111]);
        let piece = bitfield
            .zip(torrent_bitfield)
            .find(|(bt, tr)| bt.bit == 1_u8 && tr.bit == 0);

        if let Some((piece, _)) = piece {
            assert_eq!(piece, BitItem { index: 7, bit: 1 })
        }

        // ------------

        let torrent_bitfield = Bitfield::from(vec![0b1111_1111]);
        let bitfield = Bitfield::from(vec![0b1111_1111]);
        let piece = bitfield
            .zip(torrent_bitfield)
            .find(|(bt, tr)| bt.bit == 1_u8 && tr.bit == 0);

        assert_eq!(piece, None)
    }
}
