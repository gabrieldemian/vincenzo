pub mod session;
use bendy::{decoding::FromBencode, encoding::ToBencode};
use bitlab::SingleBits;
use futures::{SinkExt, StreamExt};
use hashbrown::HashSet;
use std::{
    net::SocketAddr,
    sync::{atomic::Ordering, Arc},
    time::Duration,
};
use tokio::{
    select,
    sync::{
        mpsc::{self, Receiver, Sender},
        oneshot, RwLock,
    },
    time::{interval, interval_at, Instant},
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
    peer::session::ConnectionState,
    tcp_wire::{
        lib::{Block, BlockInfo, BLOCK_LEN},
        messages::{Handshake, HandshakeCodec, Message, MessageId, PeerCodec},
    },
    torrent::{TorrentCtx, TorrentMsg},
    tracker::tracker::TrackerCtx,
};

use self::session::Session;

/// Determines who initiated the connection.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Direction {
    /// Outbound means we initiated the connection
    Outbound,
    /// Inbound means the peer initiated the connection
    Inbound,
}

/// Messages that peers can send to each other.
/// Only Torrent can send Peer messages.
#[derive(Debug)]
pub enum PeerMsg {
    DownloadedPiece(usize),
    /// Request again a block that has been requested but not sent
    RequestBlockInfo(BlockInfo),
    /// Tell this peer that we are not interested,
    /// update the local state and send a message to the peer
    NotInterested,
    Cancel(BlockInfo),
    /// Disk will send this message when the last blocks were requested
    /// The peer will then a message to Torrent with all the pending blocks
    /// The Torrent will send messages to all peers to request all pending blocks.
    StartEndgame,
    /// When the program is being gracefuly shutdown, we need to kill the tokio green thread
    /// of the peer.
    Quit,
}

/// Data about the remote Peer that we are connected to
#[derive(Debug)]
pub struct Peer {
    pub extension: Extension,
    pub reserved: [u8; 8],
    pub torrent_ctx: Arc<TorrentCtx>,
    pub tracker_ctx: Arc<TrackerCtx>,
    pub disk_tx: Sender<DiskMsg>,
    pub rx: Receiver<PeerMsg>,

    /// Context of the Peer which is shared for anyone who needs it.
    pub ctx: Arc<PeerCtx>,

    /// TCP addr that this peer is listening on
    pub addr: SocketAddr,
    pub have_info: bool,

    /// Most of the session's information and state is stored here, i.e. it's
    /// the "context" of the session, with information like: endgame mode, slow start,
    /// download_rate, etc.
    pub session: Session,

    /// Our pending requests that we sent to peer. It represents the blocks that
    /// we are expecting.
    ///
    /// If we receive a block whose request entry is here, that entry is
    /// removed. A request is also removed here when it is timed out.
    outgoing_requests: HashSet<BlockInfo>,
    /// The requests we got from peer.
    ///
    /// The request's entry is removed from here when the block is transmitted
    /// or when the peer cancels it. If a peer sends a request and cancels it
    /// before the disk read is done, the read block is dropped.
    incoming_requests: HashSet<BlockInfo>,
}

/// Ctx that is shared with Torrent and Disk;
#[derive(Debug)]
pub struct PeerCtx {
    pub tx: mpsc::Sender<PeerMsg>,
    /// a `Bitfield` with pieces that this peer
    /// has, and hasn't, containing 0s and 1s
    pub pieces: RwLock<Bitfield>,
    /// Updated when the peer sends us its peer
    /// id, in the handshake.
    pub id: RwLock<Option<[u8; 20]>>,
}

impl Peer {
    pub fn new(
        addr: SocketAddr,
        peer_tx: Sender<PeerMsg>,
        torrent_ctx: Arc<TorrentCtx>,
        rx: Receiver<PeerMsg>,
        disk_tx: Sender<DiskMsg>,
        tracker_ctx: Arc<TrackerCtx>,
    ) -> Self {
        let ctx = Arc::new(PeerCtx {
            pieces: RwLock::new(Bitfield::new()),
            id: RwLock::new(None),
            tx: peer_tx,
        });

        Peer {
            incoming_requests: HashSet::default(),
            outgoing_requests: HashSet::default(),
            session: Session::default(),
            have_info: false,
            extension: Extension::default(),
            reserved: [0_u8; 8],
            addr,
            torrent_ctx,
            disk_tx,
            tracker_ctx,
            ctx,
            rx,
        }
    }

    #[tracing::instrument(skip(self), name = "peer::start", ret)]
    pub async fn start(
        &mut self,
        direction: Direction,
        mut socket: Framed<TcpStream, HandshakeCodec>,
    ) -> Result<Framed<TcpStream, PeerCodec>, Error> {
        let our_handshake = Handshake::new(self.torrent_ctx.info_hash, self.tracker_ctx.peer_id);

        self.session.state.connection = ConnectionState::Handshaking;

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
            let mut id = self.ctx.id.write().await;
            *id = Some(their_handshake.peer_id);
            let _ = self.disk_tx.send(DiskMsg::NewPeer(self.ctx.clone())).await;
            let _ = self
                .torrent_ctx
                .tx
                .send(TorrentMsg::PeerConnected(
                    their_handshake.peer_id,
                    self.ctx.clone(),
                ))
                .await;
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
        let mut socket = Framed::from_parts(new_parts);

        // if they are connecting, answer with our extended handshake
        // if supported
        if direction == Direction::Inbound {
            if let Ok(true) = self.reserved[5].get_bit(3) {
                info!("sending extended handshake to {:?}", self.addr);

                // we need to have the info downloaded in order to send the
                // extended message, because it contains the metadata_size
                if self.have_info {
                    let info = self.torrent_ctx.info.read().await;
                    let metadata_size = info.to_bencode().map_err(|_| Error::BencodeError)?.len();
                    drop(info);

                    let ext = Extension::supported(Some(metadata_size as u32))
                        .to_bencode()
                        .map_err(|_| Error::BencodeError)?;

                    let extended = Message::Extended((0, ext));

                    socket.send(extended).await?;
                    self.maybe_request_info(&mut socket).await?;
                }
            }
        }

        // send bitfield if we have downloaded at least 1 block
        let downloaded = self.torrent_ctx.downloaded_blocks.read().await;
        if downloaded.len() > 0 {
            let bitfield = self.torrent_ctx.pieces.read().await;
            socket.send(Message::Bitfield(bitfield.clone())).await?;
            info!("sent Bitfield");
        }
        drop(downloaded);

        // todo: implement choke & interested algorithms
        // send Interested
        self.session.state.am_interested = true;
        socket.send(Message::Interested).await?;

        // send Unchoke
        self.session.state.am_choking = false;
        socket.send(Message::Unchoke).await?;

        Ok(socket)
    }

    #[tracing::instrument(skip(self, socket), name = "peer::run", ret)]
    pub async fn run(
        &mut self,
        direction: Direction,
        socket: Framed<TcpStream, PeerCodec>,
    ) -> Result<(), Error> {
        let mut tick_timer = interval(Duration::from_secs(1));

        let mut keep_alive_timer = interval_at(
            Instant::now() + Duration::from_secs(120),
            Duration::from_secs(120),
        );
        let (mut sink, mut stream) = socket.split();

        let torrent_tx = self.torrent_ctx.tx.clone();

        self.session.state.connection = ConnectionState::Connected;

        loop {
            select! {
                // update internal data every 1 second
                _now = tick_timer.tick() => {
                    self.tick(&mut sink).await?;
                }
                // send Keepalive every 2 minutes
                _ = keep_alive_timer.tick() => {
                    sink.send(Message::KeepAlive).await?;
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
                            let mut b = self.ctx.pieces.write().await;
                            *b = bitfield.clone();
                            drop(b);

                            // update the bitfield of the `Torrent`
                            // will create a new, empty bitfield, with
                            // the same len
                            let _ = torrent_tx.send(TorrentMsg::UpdateBitfield(bitfield.len_bytes()))
                                .await;

                            info!("pieces after bitfield {:?}", self.ctx.pieces);
                            info!("------------------------------\n");
                        }
                        Message::Unchoke => {
                            // self.session.peer_choking.swap(false, Ordering::Relaxed);
                            self.session.state.peer_choking = false;
                            info!("---------------------------------");
                            info!("| {:?} Unchoke  |", self.addr);
                            info!("---------------------------------");

                            if self.can_request() {
                                self.session.prepare_for_download();
                                self.request_block_infos(&mut sink).await?;
                            }
                            info!("---------------------------------\n");
                        }
                        Message::Choke => {
                            self.session.state.peer_choking = true;
                            info!("--------------------------------");
                            info!("| {:?} Choke  |", self.addr);
                            info!("---------------------------------");
                            self.free_pending_blocks().await;
                        }
                        Message::Interested => {
                            info!("------------------------------");
                            info!("| {:?} Interested  |", self.addr);
                            info!("-------------------------------");
                            self.session.state.peer_interested = true;
                        }
                        Message::NotInterested => {
                            info!("------------------------------");
                            info!("| {:?} NotInterested  |", self.addr);
                            info!("-------------------------------");
                            self.session.state.peer_interested = false;
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
                            let mut pieces = self.ctx.pieces.write().await;
                            pieces.set(piece);
                            drop(pieces);

                            // maybe become interested in peer
                            let torrent_ctx = self.torrent_ctx.clone();
                            let torrent_p = torrent_ctx.pieces.read().await;
                            let has_piece = torrent_p.get(piece);

                            match has_piece {
                                Some(_) => {
                                    info!("Already have this piece, not sending interested");
                                }
                                None => {
                                    info!("We do not have this piece, sending interested");
                                    // maybe request the incoming piece
                                    if self.can_request() {
                                        let bit_item = torrent_p.get(piece);

                                        if let Some(a) = bit_item {
                                            if a.bit == 0 {
                                                info!("requesting piece {piece}");
                                                self.request_block_infos(&mut sink).await?;
                                            }
                                        }
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

                            // ---- handle block_msg -----

                            let index = block.index;
                            let begin = block.begin;
                            let len = block.block.len();

                            let block_info = BlockInfo {
                                index: index as u32,
                                begin,
                                len: len as u32
                            };

                            // remove pending block request
                            self.outgoing_requests.remove(&block_info);

                            if self.session.in_endgame {
                                let from = self.ctx.id.read().await.unwrap();
                                let _ = torrent_tx.send(
                                    TorrentMsg::SendCancel {
                                        from,
                                        block_info: block_info.clone()
                                    }
                                )
                                .await;
                            }

                            let downloaded = self.torrent_ctx.downloaded_blocks.read().await;
                            let was_downloaded = downloaded.contains(&block_info);

                            if was_downloaded {
                                info!("already downloaded, ignoring");
                                self.session.record_waste(block_info.len);
                            }

                            drop(downloaded);
                            let is_valid = block.is_valid();

                            if is_valid && !was_downloaded {
                                info!("block not downloaded");

                                // update download stats
                                self.session.update_download_stats(block_info.len);

                                // increment downloaded count
                                self.torrent_ctx.downloaded.fetch_add(len as u64, Ordering::SeqCst);

                                let info = self.torrent_ctx.info.read().await;
                                let downloaded = self.torrent_ctx.downloaded.load(Ordering::Relaxed);
                                let is_download_complete = downloaded >= info.get_size();
                                drop(info);

                                info!("yy__downloaded {:?}", downloaded);

                                if is_download_complete {
                                    info!("download completed!! wont request more blocks");

                                    let _ = torrent_tx.send(TorrentMsg::DownloadComplete).await;
                                }

                                let (tx, rx) = oneshot::channel();

                                self.disk_tx.send(
                                    DiskMsg::WriteBlock {
                                        b: block,
                                        recipient: tx,
                                        info_hash: self.torrent_ctx.info_hash
                                    }
                                )
                                .await
                                .unwrap();

                                match rx.await.unwrap() {
                                    Ok(_) => {
                                        info!("wrote block with success on disk");
                                        let mut bd = self.torrent_ctx.downloaded_blocks.write().await;
                                        bd.insert(block_info);
                                        drop(bd);
                                    }
                                    Err(e) => warn!("could not write block to disk {e:#?}")
                                }

                                let info = self.torrent_ctx.info.read().await;

                                // if this is the last block of a piece,
                                // validate the hash
                                if begin + len as u32 >= info.piece_length {
                                    let (tx, rx) = oneshot::channel();

                                    // Ask Disk to validate the bytes of all blocks of this piece
                                    let _ = self.disk_tx.send(DiskMsg::ValidatePiece(index, tx)).await;
                                    let r = rx.await;

                                    // Hash of piece is valid
                                    if let Ok(Ok(_)) = r {
                                        let _ = torrent_tx.send(TorrentMsg::DownloadedPiece(index)).await;
                                        info!("hash of piece {index:?} is valid");

                                        let mut tr_pieces = self.torrent_ctx.pieces.write().await;

                                        tr_pieces.set(index);
                                    } else {
                                        warn!("The hash of the piece {index:?} is invalid");
                                    }
                                }
                                drop(info);
                            }
                            if !is_valid {
                                // block not valid nor requested,
                                // todo: remove it from requested blocks
                                warn!("invalid block from Piece, ignoring...");
                            }

                            if self.can_request() {
                                self.request_block_infos(&mut sink).await?;
                            }

                            info!("---------------------------------\n");
                        }
                        Message::Cancel(block_info) => {
                            info!("------------------------------");
                            info!("| {:?} Cancel  |", self.addr);
                            info!("------------------------------");
                            info!("{block_info:?}");
                            self.incoming_requests.remove(&block_info);
                        }
                        Message::Request(block_info) => {
                            info!("------------------------------");
                            info!("| {:?} Request  |", self.addr);
                            info!("------------------------------");
                            info!("{block_info:?}");
                            if !self.session.state.peer_choking {
                                let begin = block_info.begin;
                                let index = block_info.index as usize;
                                let (tx, rx) = oneshot::channel();

                                // check if peer is not already requesting this block
                                if self.incoming_requests.contains(&block_info) {
                                    // TODO: if peer keeps spamming us, close connection
                                    warn!("Peer sent duplicate block request");
                                }

                                self.incoming_requests.insert(block_info.clone());

                                let _ = self.disk_tx.send(
                                    DiskMsg::ReadBlock {
                                        b: block_info,
                                        recipient: tx,
                                        info_hash: self.torrent_ctx.info_hash,
                                    }
                                )
                                .await;

                                if let Ok(Ok(bytes)) = rx.await {
                                    let block = Block {
                                        index,
                                        begin,
                                        block: bytes,
                                    };
                                    self.torrent_ctx.uploaded.fetch_add(block.block.len() as u64, Ordering::SeqCst);
                                    let _ = sink.send(Message::Piece(block)).await;
                                }
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
                                            let info_dict = self.torrent_ctx.info_dict.read().await;
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
                                            info!("{metadata:?}");

                                            let mut info_dict = self.torrent_ctx.info_dict.write().await;

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
                                                let info_dict = self.torrent_ctx.info_dict.read().await;
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

                                                let m_info = self.torrent_ctx.magnet.xt.clone().unwrap();

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
                                                    self.have_info = true;

                                                    // update our info on torrent.info
                                                    let mut info_t = self.torrent_ctx.info.write().await;
                                                    let mut infos_t = self.torrent_ctx.block_infos.write().await;
                                                    let infos = info.get_block_infos().expect("to get block infos");
                                                    *info_t = info;
                                                    *infos_t = infos;

                                                    drop(info_t);
                                                    drop(infos_t);

                                                    let _ = self.disk_tx.send(DiskMsg::NewTorrent(self.torrent_ctx.clone())).await;

                                                    let am_interested = self.session.state.am_interested;
                                                    let peer_choking = self.session.state.peer_choking;

                                                    if am_interested && !peer_choking {
                                                        self.request_block_infos(&mut sink).await?;
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
                            info!("{:?} downloaded piece {piece}", self.addr);

                            let pieces = self.ctx.pieces.read().await;

                            if let Some(b) = pieces.get(piece) {
                                // send Have to this peer if he doesnt have this piece
                                if b.bit == 0 {
                                    info!("sending have {piece} to peer {:?}", self.addr);
                                    let _ = sink.send(Message::Have(piece)).await;
                                }
                            }
                            else if pieces.len_bytes() == 0 {
                                info!("received peer {:?} does not sent bitfield {piece}", self.addr);
                                // send also if the peer did not sent a bitfield
                                let _ = sink.send(Message::Have(piece)).await;
                            }
                            drop(pieces);
                        }
                        PeerMsg::RequestBlockInfo(info) => {
                            info!("{:?} RequestBlockInfo {info:#?}", self.addr);
                            sink.send(Message::Request(info)).await?;
                        }
                        PeerMsg::NotInterested => {
                            self.session.state.am_interested = false;
                            sink.send(Message::NotInterested).await?;
                        }
                        PeerMsg::Cancel(info) => {
                            info!("{:?} sending Cancel", self.addr);
                            sink.send(Message::Cancel(info)).await?;
                        }
                        PeerMsg::StartEndgame => {
                            info!("{:?} received StartEndgame msg", self.addr);
                            let id = self.ctx.id.read().await.unwrap();

                            for block_info in self.outgoing_requests.drain() {
                                let _ = torrent_tx.send(
                                    TorrentMsg::StartEndgame(id, block_info)
                                )
                                .await;
                            }
                        }
                        PeerMsg::Quit => {
                            info!("{:?} quitting", self.addr);
                            self.session.state.connection = ConnectionState::Quitting;
                            return Ok(());
                        }
                    }
                }
            }
        }
    }
    pub async fn tick<T>(&mut self, sink: &mut T) -> Result<(), Error>
    where
        T: SinkExt<Message> + Sized + std::marker::Unpin,
    {
        info!(
            "{:?} pending blocksss {}",
            self.addr,
            self.outgoing_requests.len()
        );
        // resent requests if we have pending requests and more time has elapsed
        // since the last request than the current timeout value
        if !self.outgoing_requests.is_empty() {
            self.check_request_timeout(sink).await?;
        }

        // update session context
        let prev_queue_len = self.session.target_request_queue_len;

        self.session.tick();

        if let (Some(prev_queue_len), Some(curr_queue_len)) =
            (prev_queue_len, self.session.target_request_queue_len)
        {
            if prev_queue_len != curr_queue_len {
                info!(
                    "{:?} request queue changed from {} to {}",
                    self.addr, prev_queue_len, curr_queue_len
                );
            }
        }

        Ok(())
    }
    /// Times out the peer if it hasn't sent a request in too long.
    async fn check_request_timeout<T>(&mut self, sink: &mut T) -> Result<(), Error>
    where
        T: SinkExt<Message> + Sized + std::marker::Unpin,
    {
        if let Some(last_outgoing_request_time) = self.session.last_outgoing_request_time {
            let elapsed_since_last_request =
                std::time::Instant::now().saturating_duration_since(last_outgoing_request_time);

            let request_timeout = self.session.request_timeout();

            info!(
                "{:?} checking request timeout \
                (last {} ms ago, timeout: {} ms)",
                self.addr,
                elapsed_since_last_request.as_millis(),
                request_timeout.as_millis()
            );

            if elapsed_since_last_request > request_timeout {
                warn!(
                    "{:?} timeout after {} ms, cancelling {} request(s) (timeouts: {})",
                    self.addr,
                    elapsed_since_last_request.as_millis(),
                    self.outgoing_requests.len(),
                    self.session.timed_out_request_count + 1
                );

                // Cancel all requests and re-issue a single one (since we can
                // only request a single block now). Start by freeing up the
                // blocks in their piece download.
                // Note that we're not telling the peer that we timed out the
                // request so that if it arrives some time later and is not
                // requested by another peer, we can still collect it.
                self.free_pending_blocks().await;
                self.session.register_request_timeout();
                self.request_block_infos(sink).await?;
            }
        }

        Ok(())
    }
    /// Marks requested blocks as free in their respective downlaods so that
    /// other peer sessions may download them.
    pub async fn free_pending_blocks(&mut self) {
        for block in self.outgoing_requests.drain() {
            // The piece may no longer be present if it was completed by
            // another peer in the meantime and torrent removed it from the
            // shared download store. This is fine, in this case we don't have
            // anything to do.
            info!("{:?} freeing block {:?} for download", self.addr, block);

            // send this block_info back to the vec of available block_infos,
            // so that other peers can download it.
            let _ = self
                .torrent_ctx
                .tx
                .send(TorrentMsg::ReturnBlockInfo(block))
                .await;
        }
    }

    #[tracing::instrument(skip(self, sink))]
    pub async fn request_block_infos<T>(&mut self, sink: &mut T) -> Result<(), Error>
    where
        T: SinkExt<Message> + Sized + std::marker::Unpin,
    {
        // the max blocks pending allowed for this peer
        let target_request_queue_len = self.session.target_request_queue_len.unwrap_or_default();

        // the number of blocks we can request right now
        let mut request_len = if self.outgoing_requests.len() >= target_request_queue_len {
            0
        } else {
            target_request_queue_len - self.outgoing_requests.len()
        };

        let available = self.torrent_ctx.block_infos.read().await;
        let available_len = available.len();
        drop(available);

        info!("available blocks {}", available_len);

        if available_len == 0 && !self.session.in_endgame && self.outgoing_requests.len() <= 20 {
            self.start_endgame().await;
        }

        // prevent from requesting more blocks than what is available
        if available_len < request_len {
            request_len = available_len;
        }

        info!("ongoing {}", self.outgoing_requests.len());
        info!("max to request {target_request_queue_len}");
        info!("requesting {request_len} blocks");

        if request_len > 0 {
            // get a list of unique block_infos from the Disk,
            // those are already marked as requested on Torrent
            let (otx, orx) = oneshot::channel();
            let _ = self
                .disk_tx
                .send(DiskMsg::RequestBlocks {
                    recipient: otx,
                    qnt: request_len,
                    info_hash: self.torrent_ctx.info_hash,
                    peer_id: self.ctx.id.read().await.unwrap(),
                })
                .await;

            let r = orx.await.unwrap();

            self.session.last_outgoing_request_time = Some(std::time::Instant::now());

            for info in r {
                info!("{:?} requesting {info:#?}", self.addr);
                self.outgoing_requests.insert(info.clone());

                let _ = sink.send(Message::Request(info)).await;
                let req_id: u64 = MessageId::Request as u64;

                self.session.counters.protocol.up += req_id;
            }
        }

        Ok(())
    }
    #[tracing::instrument(skip(self))]
    pub async fn start_endgame(&mut self) {
        info!("{:?} started endgame mode", self.addr);
        self.session.in_endgame = true;
        let id = self.ctx.id.read().await;
        debug_assert!(id.is_some());

        if let Some(id) = *id {
            for req in self.outgoing_requests.iter() {
                info!("requesting for endgame {req:#?}");
                let _ = self
                    .torrent_ctx
                    .tx
                    .send(TorrentMsg::StartEndgame(id, req.clone()))
                    .await;
            }
        }
    }

    #[tracing::instrument(skip(self, sink))]
    pub async fn maybe_request_info<T>(&mut self, sink: &mut T) -> Result<(), Error>
    where
        T: SinkExt<Message> + Sized + std::marker::Unpin,
    {
        info!("called maybe_request_info");
        let torrent_ctx = self.torrent_ctx.as_ref();
        let info_dict = torrent_ctx.info_dict.read().await;
        info!("have info? {}", self.have_info);
        drop(info_dict);

        // only request info if we dont have an Info
        // and the peer supports the metadata extension protocol
        if !self.have_info {
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
                    let _ = sink.send(Message::Extended((ut_metadata, h))).await;
                }
            }
        }
        Ok(())
    }
    /// If we can request new blocks
    pub fn can_request(&self) -> bool {
        let am_interested = self.session.state.am_interested;
        let am_choking = self.session.state.am_choking;

        am_interested && !am_choking && self.have_info
    }
}
