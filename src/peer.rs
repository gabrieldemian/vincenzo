use bendy::{decoding::FromBencode, encoding::ToBencode};
use bitlab::SingleBits;
use futures::{stream::SplitSink, SinkExt, StreamExt};
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::Arc,
    time::Duration,
};
use tokio::{
    select,
    sync::{mpsc::Sender, oneshot},
    time::Instant,
};
use tokio_util::codec::{Framed, FramedParts};

use tokio::{net::TcpStream, time::interval};
use tracing::{debug, info, warn};

use crate::{
    bitfield::Bitfield,
    disk::DiskMsg,
    error::Error,
    extension::{Extension, Metadata},
    magnet_parser::get_info_hash,
    metainfo::{Info, MetaInfo},
    tcp_wire::{
        lib::{Block, BLOCK_LEN},
        messages::{Handshake, HandshakeCodec, Message, PeerCodec},
    },
    torrent::{TorrentCtx, TorrentMsg},
    tracker::tracker::TrackerCtx,
};

/// Determines who initiated the connection.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Direction {
    /// Outbound means we initiated the connection
    Outbound,
    /// Inbound means the peer initiated the connection
    Inbound,
}

/// Data about the remote Peer that we are connected to
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
    pub reserved: [u8; 8],
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
            reserved: [0_u8; 8],
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
    #[tracing::instrument(skip(self, tx, direction, socket), name = "peer::run", ret)]
    pub async fn run(
        &mut self,
        tx: Sender<TorrentMsg>,
        direction: Direction,
        mut socket: Framed<TcpStream, HandshakeCodec>,
    ) -> Result<(), Error> {
        let torrent_ctx = self.torrent_ctx.clone().unwrap();
        let tracker_ctx = self.tracker_ctx.clone();
        let xt = torrent_ctx.magnet.xt.as_ref().unwrap();

        let info_hash = get_info_hash(xt);
        let our_handshake = Handshake::new(info_hash, tracker_ctx.peer_id);

        // we are connecting, send the first handshake
        if direction == Direction::Outbound {
            socket.send(our_handshake.clone()).await?;
        }

        // wait for, and validate, their handshake
        if let Some(their_handshake) = socket.next().await {
            info!("received handshake from {:#?}", self.addr);

            let their_handshake = their_handshake?;

            if !their_handshake.validate(&our_handshake) {
                return Err(Error::HandshakeInvalid);
            }

            self.id = Some(their_handshake.peer_id);
            self.reserved = their_handshake.reserved;
        }

        // if they are connecting, answer with our handshake
        if direction == Direction::Inbound {
            socket.send(our_handshake).await?;
        }

        let old_parts = socket.into_parts();
        let mut new_parts = FramedParts::new(old_parts.io, PeerCodec);

        new_parts.read_buf = old_parts.read_buf;
        new_parts.write_buf = old_parts.write_buf;

        let socket = Framed::from_parts(new_parts);

        let (mut sink, mut stream) = socket.split();

        // check if peer supports Extended Protocol
        if let Ok(true) = self.reserved[5].get_bit(3) {
            // extended handshake must be sent after the handshake
            sink.send(Message::Extended((0, vec![]))).await?;
        }

        // Send Interested & Unchoke to peer
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
                            if f.is_none() {
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

                            let bd = torrent_ctx.downloaded_blocks.read().await;
                            let was_downloaded = bd.iter().any(|b| *b == block.clone().into() );
                            drop(bd);

                            if block.is_valid() && !was_downloaded {
                                let (tx, rx) = oneshot::channel();
                                let disk_tx = self.disk_tx.as_ref().unwrap();

                                disk_tx.send(DiskMsg::WriteBlock((block.clone(), tx))).await.unwrap();

                                let r = rx.await.unwrap();

                                match r {
                                    Ok(_) => {
                                        let mut bd = torrent_ctx.downloaded_blocks.write().await;
                                        bd.push_front(block.clone().into());
                                        drop(bd);

                                        info!("wrote piece with success on disk");
                                    }
                                    Err(e) => warn!("could not write piece to disk {e:#?}")
                                }

                                let info = torrent_ctx.info.read().await;

                                // if this is the last block of a piece
                                if block.begin + block.block.len() as u32 >= info.piece_length {
                                    drop(info);
                                    let (tx, rx) = oneshot::channel();
                                    let disk_tx = self.disk_tx.as_ref().unwrap();

                                    // Ask Disk to validate the bytes of all blocks of this piece
                                    let _ = disk_tx.send(DiskMsg::ValidatePiece((block.index, tx))).await;
                                    let r = rx.await;

                                    // Hash of piece is valid
                                    if let Ok(Ok(_r)) = r {
                                        info!("hash of piece {:?} is valid", block.index);
                                        let mut tr_pieces = torrent_ctx.pieces.write().await;
                                        tr_pieces.set(block.index);
                                        // todo: send Have msg to peers that dont have this piece
                                    } else {
                                        warn!("The hash of the piece {:?} is invalid", block.index);
                                    }
                                }
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

                            info!("ext_id {ext_id}");

                            let info_dict = torrent_ctx.info_dict.read().await;
                            let no_info = info_dict.is_empty();
                            drop(info_dict);

                            // only request info if we dont have an Info
                            // and the peer supports the metadata extension protocol
                            if ext_id == 0 && no_info && self.extension.m.ut_metadata.is_none() {
                                let extension = Extension::from_bencode(&payload).map_err(|_| Error::BencodeError)?;
                                self.extension = extension;

                                // if peer is requesting, send or reject
                                // if let Ok(metadata) = Metadata::from_bencode(&payload) {
                                //     if metadata.msg_type == 0 {
                                //         let info_dict = torrent_ctx.info_dict.read().await;
                                //         let piece = info_dict.get(&(metadata.msg_type as u32));
                                //
                                //         match piece {
                                //             Some(p) => {
                                //                 let meta = MetaInfo::from_bencode(p).unwrap();
                                //                 let r = Metadata::data(metadata.msg_type as u32, meta.info)?;
                                //                 let r = r.to_bencode().map_err(|_| Error::BencodeError)?;
                                //                 sink.send(Message::Extended((0, r))).await?;
                                //             }
                                //             None => {
                                //                 let r = Metadata::reject(metadata.msg_type as u32).to_bencode()
                                //                 .map_err(|_| Error::BencodeError)?;
                                //                 sink.send(Message::Extended((0, r))).await?;
                                //             }
                                //         }
                                //     }
                                // }

                                // send bep09 request to get the Info
                                if let Some(ut_metadata) = self.extension.m.ut_metadata {
                                    info!("peer supports ut_metadata {ut_metadata}, sending request");

                                    let t = self.extension.metadata_size.unwrap();
                                    let pieces = t as f32 / BLOCK_LEN as f32;
                                    let pieces = pieces.ceil() as u32 ;

                                    for i in 0..pieces {
                                        let h = Metadata::request(i).to_bencode().map_err(|_| Error::BencodeError)?;

                                        info!("sending request msg with ut_metadata {ut_metadata:?}");
                                        sink.send(Message::Extended((ut_metadata, h))).await?;
                                    }
                                }
                            }

                            // ext_id of 0 means a bep10 extended handshake
                            info!("payload len {:?}", payload.len());
                            if ext_id == 0 && Some(payload.len() as u32) >= self.extension.metadata_size {
                                info!("extension {:?}", self.extension);

                                // the payload is the only msg that is larger than the info len
                                // we can safely assume this is a data msg with the info bytes
                                if self.extension.m.ut_metadata.is_some() {
                                    info!("received data msg");
                                    // can be a peer sending the handshake,
                                    // or the response of a request (a data)
                                    let t = self.extension.metadata_size.unwrap();
                                    let info_begin = payload.len() - t as usize;

                                    let pieces = t as f32 / BLOCK_LEN as f32;
                                    let pieces = pieces.ceil() as u32 ;

                                    let (metadata, info) = Metadata::extract(payload, info_begin)?;

                                    let mut info_dict = torrent_ctx.info_dict.write().await;

                                    info_dict.insert(metadata.piece, info);
                                    drop(info_dict);

                                    let info_dict = torrent_ctx.info_dict.write().await;
                                    let have_all_pieces = info_dict.keys().count() as u32 >= pieces;
                                    info!("have_all_pieces {have_all_pieces:?}");

                                    // if this is the last piece
                                    if have_all_pieces {
                                        let info_bytes = info_dict.values().fold(Vec::new(), |mut acc, b| {
                                            acc.extend_from_slice(b);
                                            acc
                                        });

                                        drop(info_dict);

                                        // info has a valid bencode format
                                        let info = Info::from_bencode(&info_bytes).map_err(|_| Error::BencodeError)?;
                                        info!("downloaded full Info from peer {:?}", self.addr);

                                        let m_info = torrent_ctx.magnet.xt.clone().unwrap();

                                        let mut hash = sha1_smol::Sha1::new();
                                        hash.update(&info_bytes);

                                        let hash = hash.digest().bytes();

                                        // validate the hash of the downloaded info
                                        // against the hash of the magnet link
                                        let hash = hex::encode(hash);
                                        info!("hash hex: {hash:?}");

                                        if hash == m_info {
                                            info!("the hash of the downloaded info matches the hash of the magnet link");
                                            // update our info on torrent.info
                                            let mut info_t = torrent_ctx.info.write().await;
                                            *info_t = info.clone();
                                            drop(info_t);

                                            let _ = self.disk_tx.as_ref().unwrap().send(DiskMsg::NewTorrent(info)).await;
                                            if self.am_interested && !self.peer_choking {
                                                self.request_next_piece(&mut sink).await?;
                                            }
                                        } else {
                                            warn!("the peer {:?} sent a valid Info, but the hash does not match the hash of the provided magnet link, panicking", self.addr);
                                            panic!();
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
    use crate::tcp_wire::lib::{Block, BLOCK_LEN};

    use super::*;

    #[test]
    fn can_get_requested_blocks_but_not_downloaded() {
        let requested = vec![0, 1, 2, 3, 4, 5, 6];
        let downloaded = [0, 1, 2, 3];
        let mut missing: Vec<i32> = Vec::new();

        for req in &*requested {
            let f = downloaded.iter().find(|down| **down == *req);
            if f.is_none() {
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
}
