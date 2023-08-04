use bendy::{decoding::FromBencode, encoding::ToBencode};
use bitlab::SingleBits;
use futures::{stream::SplitSink, SinkExt, StreamExt};
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{atomic::Ordering, Arc},
    time::Duration,
};
use tokio::{
    select,
    sync::{
        mpsc::{self, Receiver, Sender},
        oneshot,
    },
    time::{interval_at, Instant},
};
use tokio_util::codec::{Framed, FramedParts};

use tokio::net::TcpStream;
use tracing::{info, warn};

use crate::{
    bitfield::Bitfield,
    disk::DiskMsg,
    error::Error,
    extension::{Extension, Metadata},
    metainfo::Info,
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
#[derive(Debug)]
pub struct Peer {
    /// if this client is choking this peer
    pub am_choking: bool,
    /// if this client is interested in this peer
    pub am_interested: bool,
    /// if this peer is choking the client
    pub peer_choking: bool,
    /// if this peer is interested in the client
    pub peer_interested: bool,
    pub extension: Extension,
    /// Updated when the peer sends us its peer
    /// id, in the handshake.
    pub id: Option<[u8; 20]>,
    pub reserved: [u8; 8],
    pub torrent_ctx: Arc<TorrentCtx>,
    pub torrent_tx: mpsc::Sender<TorrentMsg>,
    pub tracker_ctx: Arc<TrackerCtx>,
    pub disk_tx: Option<Sender<DiskMsg>>,
    pub rx: Receiver<PeerMsg>,
    /// a `Bitfield` with pieces that this peer
    /// has, and hasn't, containing 0s and 1s
    pub pieces: Bitfield,
    pub ctx: PeerCtx,
    /// TCP addr that this peer is listening on
    pub addr: SocketAddr,
}

/// Messages that peers can send to each other.
/// Only [Torrent] can send Peer messages.
#[derive(Debug)]
pub enum PeerMsg {
    DownloadedPiece(usize),
}

/// Ctx that is shared with [Torrent];
#[derive(Debug, Clone)]
pub struct PeerCtx {
    pub peer_tx: mpsc::Sender<PeerMsg>,
    /// TCP addr that this peer is listening on
    pub addr: String,
}

impl Peer {
    pub fn new(
        peer_tx: Sender<PeerMsg>,
        torrent_tx: Sender<TorrentMsg>,
        torrent_ctx: Arc<TorrentCtx>,
        rx: Receiver<PeerMsg>,
    ) -> Self {
        let ctx = PeerCtx {
            peer_tx,
            addr: "".to_owned(),
        };

        Peer {
            torrent_tx,
            pieces: Bitfield::new(),
            am_choking: true,
            am_interested: false,
            extension: Extension::default(),
            peer_choking: true,
            peer_interested: false,
            addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0),
            id: None,
            torrent_ctx,
            disk_tx: None,
            tracker_ctx: Arc::new(TrackerCtx::default()),
            reserved: [0_u8; 8],
            ctx,
            rx,
        }
    }
    pub fn addr(mut self, addr: SocketAddr) -> Self {
        self.addr = addr;
        self.ctx.addr = self.addr.to_string();
        self
    }
    pub fn torrent_ctx(mut self, ctx: Arc<TorrentCtx>) -> Self {
        self.torrent_ctx = ctx;
        self
    }
    pub fn id(mut self, id: [u8; 20]) -> Self {
        self.id = Some(id);
        self
    }
    pub fn tracker_ctx(mut self, tracker_ctx: Arc<TrackerCtx>) -> Self {
        self.tracker_ctx = tracker_ctx;
        self
    }
    pub fn disk_tx(mut self, disk_tx: Sender<DiskMsg>) -> Self {
        self.disk_tx = Some(disk_tx);
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
        let torrent_ctx = self.torrent_ctx.clone();
        let tr_pieces = torrent_ctx.pieces.read().await;

        let disk_tx = self.disk_tx.clone().unwrap();

        info!("downloaded {tr_pieces:?}");
        info!("tr_pieces len: {:?}", tr_pieces.len());
        info!("self.pieces: {:?}", self.pieces);
        info!("self.pieces len: {:?}", self.pieces.len());

        // get a list of unique block_infos from the Disk,
        // those are already marked as requested on Torrent
        let (otx, orx) = oneshot::channel();
        let _ = disk_tx
            .send(DiskMsg::RequestBlocks {
                recipient: otx,
                qnt: 1,
                pieces: self.pieces.clone(),
            })
            .await;

        let r = orx.await.unwrap();

        for info in r {
            info!("requesting to {:?} {info:#?}", self.addr);
            sink.send(Message::Request(info)).await?;
        }

        Ok(())
    }
    #[tracing::instrument(skip(self, sink))]
    pub async fn maybe_request_info(
        &mut self,
        sink: &mut SplitSink<Framed<TcpStream, PeerCodec>, Message>,
    ) -> Result<(), Error> {
        info!("called maybe_request_info");
        let torrent_ctx = self.torrent_ctx.as_ref().clone();
        let info_dict = torrent_ctx.info_dict.read().await;
        let no_info = info_dict.is_empty();
        info!("no info? {no_info}");
        drop(info_dict);

        // only request info if we dont have an Info
        // and the peer supports the metadata extension protocol
        if no_info {
            // send bep09 request to get the Info
            if let Some(ut_metadata) = self.extension.m.ut_metadata {
                info!("peer supports ut_metadata {ut_metadata}, sending request");

                let t = self.extension.metadata_size.unwrap();
                let pieces = t as f32 / BLOCK_LEN as f32;
                let pieces = pieces.ceil() as u32;
                info!("this info has {pieces} pieces");

                for i in 0..pieces {
                    let h = Metadata::request(i);

                    info!("requesting info piece {i}");
                    info!("request {h:?}");

                    let h = h.to_bencode().map_err(|_| Error::BencodeError)?;
                    sink.send(Message::Extended((ut_metadata, h))).await?;
                }
            }
        }
        Ok(())
    }
    #[tracing::instrument(skip(self, direction, socket), name = "peer::run", ret)]
    pub async fn run(
        &mut self,
        direction: Direction,
        mut socket: Framed<TcpStream, HandshakeCodec>,
    ) -> Result<(), Error> {
        info!("entered run fn");
        let torrent_ctx = self.torrent_ctx.clone();
        let tracker_ctx = self.tracker_ctx.clone();

        let our_handshake = Handshake::new(torrent_ctx.info_hash, tracker_ctx.peer_id);

        // we are connecting, send the first handshake
        if direction == Direction::Outbound {
            info!("sending the first handshake, outbound");
            socket.send(our_handshake.clone()).await?;
        }

        // wait for, and validate, their handshake
        if let Some(their_handshake) = socket.next().await {
            info!("-------------------------------------");
            info!("| {:?} Handshake  |", self.addr);
            info!("-------------------------------------");
            info!("{direction:#?}");

            let their_handshake = their_handshake?;

            if !their_handshake.validate(&our_handshake) {
                return Err(Error::HandshakeInvalid);
            }
            self.id = Some(their_handshake.peer_id);
            self.reserved = their_handshake.reserved;
        }

        // if they are connecting, answer with our handshake
        if direction == Direction::Inbound {
            info!("sending the second handshake");
            socket.send(our_handshake).await?;
        }

        let old_parts = socket.into_parts();
        let mut new_parts = FramedParts::new(old_parts.io, PeerCodec);
        new_parts.read_buf = old_parts.read_buf;
        new_parts.write_buf = old_parts.write_buf;
        let socket = Framed::from_parts(new_parts);
        let (mut sink, mut stream) = socket.split();

        // if they are connecting, answer with our extended handshake
        // if supported
        if direction == Direction::Inbound {
            if let Ok(true) = self.reserved[5].get_bit(3) {
                info!("sending extended handshake to {:?}", self.addr);
                let info = torrent_ctx.info.read().await;

                if info.piece_length > 0 {
                    let metadata_size = info.to_bencode().map_err(|_| Error::BencodeError)?.len();

                    let ext = Extension::supported(Some(metadata_size as u32))
                        .to_bencode()
                        .map_err(|_| Error::BencodeError)?;

                    let extended = Message::Extended((0, ext));

                    sink.send(extended).await?;
                    self.maybe_request_info(&mut sink).await?;
                }
            }
        }

        // send bitfield if we have downloaded at least 1 block
        let downloaded = torrent_ctx.downloaded_blocks.read().await;
        if downloaded.len() > 0 {
            let bitfield = torrent_ctx.pieces.read().await;
            sink.send(Message::Bitfield(bitfield.clone())).await?;
            info!("sent Bitfield");
        }
        drop(downloaded);

        // Send Interested & Unchoke to peer
        sink.send(Message::Interested).await?;
        self.am_interested = true;
        sink.send(Message::Unchoke).await?;
        self.am_choking = false;

        let mut keep_alive_timer = interval_at(
            Instant::now() + Duration::from_secs(120),
            Duration::from_secs(120),
        );
        let mut request_timer = interval_at(
            Instant::now() + Duration::from_secs(10),
            Duration::from_secs(10),
        );

        loop {
            select! {
                _ = keep_alive_timer.tick() => {
                    // send Keepalive every 2 minutes
                    sink.send(Message::KeepAlive).await?;
                }
                // Sometimes that blocks requested are never sent to us,
                // this can happen for a lot of reasons, and is normal and expected.
                // The algorithm to re-request those blocks is simple:
                // At every 5 seconds, check for blocks that were requested,
                // but not downloaded, and request them again.
                _ = request_timer.tick() => {
                    // let requested = torrent_ctx.requested_blocks.read().await;
                    // let front = requested.front();
                    // println!("front requested bi {front:#?}");
                    // drop(requested);

                    // let downloaded = torrent_ctx.downloaded_blocks.read().await;
                    // let front = downloaded.front();
                    // println!("front downloaded bi {front:#?}");
                    // drop(downloaded);

                    // let info = torrent_ctx.info.read().await;
                    // // we know if the info is downloaded if the piece_length is > 0
                    // if info.piece_length > 0 {
                    //     // check if there is requested blocks that hasnt been downloaded
                    //     let downloaded_blocks = torrent_ctx.downloaded_blocks.read().await;
                    //     let requested = torrent_ctx.requested_blocks.read().await;
                    //
                    //     let downloaded = torrent_ctx.downloaded.load(Ordering::SeqCst);
                    //     let info = torrent_ctx.info.read().await;
                    //     let torrent_size = info.get_size();
                    //
                    //     // if our torrent has not been completely downloaded
                    //     if downloaded < torrent_size {
                    //         for req in &*requested {
                    //             let f = downloaded_blocks.iter().find(|down| **down == *req);
                    //             if f.is_none() {
                    //                 info!("request block but not received {req:#?}");
                    //                 let _ = sink.send(Message::Request(req.clone())).await;
                    //             }
                    //         }
                    //     } else {
                    //         info!("no more blocks to download.");
                    //         request_timer = interval_at(
                    //             Instant::now() + Duration::from_secs(u32::MAX.into()),
                    //             Duration::from_secs(u64::MAX),
                    //         );
                    //     }
                    // }
                    // drop(info);
                }
                Some(Ok(msg)) = stream.next() => {
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
                            let _ = self.torrent_tx.send(TorrentMsg::UpdateBitfield(bitfield.len_bytes()))
                                .await;

                            info!("pieces after bitfield {:?}", self.pieces);
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
                            info!("-------------------------------");
                            info!("| {:?} Have {piece}  |", self.addr);
                            info!("-------------------------------");
                            // Have is usually sent when the peer has downloaded
                            // a new piece, however, some peers, after handshake,
                            // send an incomplete bitfield followed by a sequence of
                            // have's. They do this to try to prevent censhorship
                            // from ISPs.
                            // Overwrite pieces on bitfield, if the peer has one
                            self.pieces.set(piece);

                            // maybe request the incoming piece
                            if self.am_interested && !self.peer_choking {
                                let tr_pieces = self.torrent_ctx.pieces.read().await;
                                let bit_item = tr_pieces.get(piece);
                                drop(tr_pieces);

                                if let Some(a) = bit_item {
                                    if a.bit == 0 {
                                        info!("requesting piece {piece}");
                                        self.request_next_piece(&mut sink).await?;
                                    }
                                }
                            }
                        }
                        Message::Piece(block) => {
                            info!("-------------------------------");
                            info!("| {:?} Piece {}  |", self.addr, block.index);
                            info!("-------------------------------");
                            info!("index: {:?}", block.index);
                            info!("begin: {:?}", block.begin);
                            info!("len: {:?}", block.block.len());
                            info!("--");

                            let bd = torrent_ctx.downloaded_blocks.read().await;
                            let was_downloaded = bd.iter().any(|b| *b == block.clone().into() );
                            drop(bd);

                            if was_downloaded {
                                info!("this block is already downloaded, ignoring");
                            }

                            if block.is_valid() && !was_downloaded {
                                let (tx, rx) = oneshot::channel();
                                let disk_tx = self.disk_tx.as_ref().unwrap();

                                disk_tx.send(DiskMsg::WriteBlock((block.clone(), tx))).await.unwrap();

                                let r = rx.await.unwrap();

                                match r {
                                    Ok(_) => {
                                        torrent_ctx.downloaded.fetch_add(block.block.len() as u64, Ordering::SeqCst);
                                        let mut bd = torrent_ctx.downloaded_blocks.write().await;
                                        bd.push_front(block.clone().into());
                                        drop(bd);

                                        info!("wrote piece with success on disk");
                                    }
                                    Err(e) => warn!("could not write piece to disk {e:#?}")
                                }

                                let info = self.torrent_ctx.info.read().await;
                                let piece_length = info.piece_length;
                                let block_len = block.block.len() as u32 + block.begin;
                                let last_blocks_len = info.get_last_blocks_len();

                                // if this is the last block of a piece
                                // if block_len >= piece_length {
                                if block_len == piece_length || last_blocks_len.iter().find(|x| **x == block.block.len() as u32).is_some() {
                                    let (tx, rx) = oneshot::channel();
                                    let disk_tx = self.disk_tx.as_ref().unwrap();

                                    // Ask Disk to validate the bytes of all blocks of this piece
                                    let _ = disk_tx.send(DiskMsg::ValidatePiece((block.index, tx))).await;
                                    let r = rx.await;

                                    // Hash of piece is valid
                                    if let Ok(Ok(_)) = r {
                                        let _ = self.torrent_tx.send(TorrentMsg::DownloadedPiece(block.index)).await;
                                        info!("hash of piece {:?} is valid", block.index);

                                        let mut tr_pieces = torrent_ctx.pieces.write().await;

                                        tr_pieces.set(block.index);
                                        drop(tr_pieces);
                                    } else {
                                        warn!("The hash of the piece {:?} is invalid", block.index);
                                    }
                                }
                            }

                            if !block.is_valid() {
                                // block not valid nor requested,
                                // remove it from requested blocks
                                warn!("invalid block from Piece, ignoring...");
                            }

                            let info = torrent_ctx.info.read().await;
                            let is_download_complete = torrent_ctx.downloaded.load(Ordering::SeqCst) >= info.get_size();
                            drop(info);

                            if self.am_interested && !self.peer_choking && !is_download_complete {
                                self.request_next_piece(&mut sink).await?;
                            }

                            if is_download_complete {
                                info!("download completed!! won't request more blocks");
                                let _ = self.torrent_tx.send(TorrentMsg::DownloadComplete).await;
                            }

                            info!("---------------------------------\n");
                        }
                        Message::Cancel(block_info) => {
                            info!("------------------------------");
                            info!("| {:?} Cancel  |", self.addr);
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
                                torrent_ctx.uploaded.fetch_add(block.block.len() as u64, Ordering::SeqCst);
                                let _ = sink.send(Message::Piece(block)).await;
                            }
                        }
                        Message::Extended((ext_id, payload)) => {

                            // receive extended handshake, send our extended handshake
                            // and maybe request info pieces if we don't have
                            if ext_id == 0 {
                                info!("--------------------------------------------");
                                info!("| {:?} Extended Handshake  |", self.addr);
                                info!("--------------------------------------------");
                                info!("ext_id {ext_id}");
                                info!("self ut_metadata {:?}", self.extension.m.ut_metadata);
                                info!("payload len {:?}", payload.len());

                                if let Ok(extension) = Extension::from_bencode(&payload) {
                                    self.extension = extension;
                                    info!("self ut_metadata {:?}", self.extension.m.ut_metadata);
                                    info!("{:?}", self.extension);

                                    if direction == Direction::Outbound {
                                        info!("outbound, sending extended handshake to {:?}", self.addr);
                                        let metadata_size = self.extension.metadata_size.unwrap();
                                        info!("metadata_size {metadata_size:?}");

                                        let ext = Extension::supported(Some(metadata_size))
                                            .to_bencode()
                                            .map_err(|_| Error::BencodeError)?;

                                        let msg = Message::Extended((0, ext));

                                        sink.send(msg).await?;
                                        self.maybe_request_info(&mut sink).await?;
                                    }
                                }
                            }

                            match self.extension.m.ut_metadata {
                                // when we send msgs, use the ext_id of the peer
                                // when we receive msgs, ext_id equals to our ext_id (3)
                                // if outbound, the peer will set ext_id to MY ut_metadata
                                // which is 3
                                // if inbound, i send the data with the ext_id of THE PEER
                                Some(ut_metadata) if ext_id == 3 => {
                                    let t = self.extension.metadata_size.unwrap();
                                    let (metadata, info) = Metadata::extract(payload.clone())?;

                                    match metadata.msg_type {
                                        // if peer is requesting, send or reject
                                        0 => {
                                            info!("-------------------------------------");
                                            info!("| {:?} Metadata Req  |", self.addr);
                                            info!("-------------------------------------");
                                            info!("ext_id {ext_id}");
                                            info!("self ut_metadata {:?}", self.extension.m.ut_metadata);
                                            info!("payload len {:?}", payload.len());
                                            let info_dict = torrent_ctx.info_dict.read().await;
                                            let info_slice = info_dict.get(&(metadata.msg_type as u32));

                                            match info_slice {
                                                Some(info_slice) => {
                                                    info!("sending data with piece {:?}", metadata.piece);
                                                    let r = Metadata::data(metadata.piece, info_slice)?;
                                                    sink.send(
                                                        Message::Extended((ut_metadata, r))
                                                    ).await?;
                                                }
                                                None => {
                                                    info!("sending reject");
                                                    let r = Metadata::reject(metadata.piece).to_bencode()
                                                        .map_err(|_| Error::BencodeError)?;
                                                    sink.send(
                                                        Message::Extended((ut_metadata, r))
                                                    ).await?;
                                                }
                                            }
                                        }
                                        1 => {
                                            info!("-------------------------------------");
                                            info!("| {:?} Metadata Res {}  |", self.addr, metadata.piece);
                                            info!("-------------------------------------");
                                            info!("ext_id {ext_id}");
                                            info!("self ut_metadata {:?}", self.extension.m.ut_metadata);
                                            info!("t {:?}", t);
                                            info!("payload len {:?}", payload.len());
                                            info!("info len {:?}", info.len());
                                            info!("info {:?}", String::from_utf8_lossy(&info[0..50]));
                                            info!("{metadata:?}");

                                            let mut info_dict = torrent_ctx.info_dict.write().await;

                                            info_dict.insert(metadata.piece, info);

                                            let info_len = info_dict.values().fold(0, |acc, b| {
                                                acc + b.len()
                                            });
                                            drop(info_dict);

                                            let have_all_pieces = info_len as u32 >= t;

                                            info!("downloaded info bytes {:?}", info_len);
                                            info!("do we have full info downloaded? {have_all_pieces:?}");

                                            // if this is the last piece
                                            if have_all_pieces {
                                                // info has a valid bencode format
                                                let info_dict = torrent_ctx.info_dict.read().await;
                                                let info_bytes = info_dict.values().fold(Vec::new(), |mut acc, b| {
                                                    acc.extend_from_slice(b);
                                                    acc
                                                });
                                                drop(info_dict);
                                                info!("info_bytes len {:?}", info_bytes.len());
                                                let info = Info::from_bencode(&info_bytes).map_err(|_| Error::BencodeError)?;
                                                info!("piece len in bytes {:?}", info.piece_length);
                                                info!("blocks per piece {:?}", info.blocks_per_piece());
                                                info!("pieces {:?}", info.pieces.len() / 20);

                                                let m_info = torrent_ctx.magnet.xt.clone().unwrap();

                                                let mut hash = sha1_smol::Sha1::new();
                                                hash.update(&info_bytes);

                                                let hash = hash.digest().bytes();

                                                // validate the hash of the downloaded info
                                                // against the hash of the magnet link
                                                let hash = hex::encode(hash);
                                                info!("hash hex: {hash:?}");
                                                info!("hash metainfo: {m_info:?}");

                                                if hash.to_uppercase() == m_info.to_uppercase() {
                                                    info!("the hash of the downloaded info matches the hash of the magnet link");

                                                    // update our info on torrent.info
                                                    let mut info_t = torrent_ctx.info.write().await;
                                                    *info_t = info.clone();

                                                    // update the vec of last blocks len
                                                    let mut lbl = torrent_ctx.last_blocks_len.write().await;
                                                    info!("the list of last block len is {lbl:#?}");

                                                    *lbl = info_t.get_last_blocks_len();
                                                    drop(info_t);
                                                    drop(lbl);

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
                                        _ => {}
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                }
                Some(msg) = self.rx.recv() => {
                    match msg {
                        PeerMsg::DownloadedPiece(piece) => {
                            info!("received peer {:?} downloaded piece {piece}", self.addr);
                            if let Some(b) = self.pieces.get(piece) {

                                // send Have to this peer if he doesnt have this piece
                                if b.bit == 0 {
                                    info!("sending have {piece} to peer {:?}", self.addr);
                                    let _ = sink.send(Message::Have(piece)).await;
                                }
                            }
                            else if self.pieces.len_bytes() == 0 {
                                info!("received peer {:?} does not sent bitfield {piece}", self.addr);
                                // send also if the peer did not sent a bitfield
                                let _ = sink.send(Message::Have(piece)).await;
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
