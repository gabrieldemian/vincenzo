pub mod session;
use bendy::{decoding::FromBencode, encoding::ToBencode};
use bitlab::SingleBits;
use futures::{SinkExt, StreamExt};
use hashbrown::HashSet;
use std::{collections::VecDeque, net::SocketAddr, sync::Arc, time::Duration};
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
use tracing::{info, warn, field::debug, debug};

use crate::{
    bitfield::Bitfield,
    disk::DiskMsg,
    error::Error,
    extension::{Extension, Metadata},
    peer::session::ConnectionState,
    tcp_wire::{
        messages::{Handshake, HandshakeCodec, Message, MessageId, PeerCodec},
        {Block, BlockInfo, BLOCK_LEN},
    },
    torrent::{TorrentCtx, TorrentMsg},
    tracker::TrackerCtx,
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
    /// When we download a full piece, we need to send Have's
    /// to peers that dont Have it.
    HavePiece(usize),
    /// Request again a block that has been requested but not sent to us
    RequestBlockInfo(BlockInfo),
    RequestBlockInfos(Vec<BlockInfo>),
    /// Tell this peer that we are not interested,
    /// update the local state and send a message to the peer
    NotInterested,
    CancelBlock(BlockInfo),
    CancelMetadata(u32),
    /// Sometimes a peer either takes too long to answer,
    /// or simply does not answer at all. In both cases
    /// we need to request the block again.
    ReRequest(BlockInfo),
    /// Sent when the torrent has downloaded the entire info
    HaveInfo,
    Pause,
    Resume,
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
    /// This is a cache of Torrent::have_info,
    /// to avoid using locks or atomics.
    have_info: bool,
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

    #[tracing::instrument(skip(self, socket), name = "peer::start", ret)]
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

        // send bitfield
        let bitfield = self.torrent_ctx.pieces.read().await;
        if bitfield.len_bytes() > 0 {
            socket.send(Message::Bitfield(bitfield.clone())).await?;
        }

        // todo: implement choke algorithm
        // send Unchoke
        self.session.state.am_choking = false;
        socket.send(Message::Unchoke).await?;

        Ok(socket)
    }

    #[tracing::instrument(skip(self, socket, direction), name = "peer::run")]
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

        self.session.state.connection = ConnectionState::Connected;

        loop {
            select! {
                // update internal data every 1 second
                _ = tick_timer.tick(), if self.have_info => {
                    self.tick(&mut sink).await?;
                }
                // send Keepalive every 2 minutes
                _ = keep_alive_timer.tick(), if self.have_info => {
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

                            for x in bitfield.into_iter() {
                                if x.bit == 0 {
                                    info!("{:?} we are interested due to Bitfield", self.addr);

                                    self.session.state.am_interested = true;
                                    sink.send(Message::Interested).await?;

                                    if self.can_request() {
                                        self.request_block_infos(&mut sink).await?;
                                    }

                                    break;
                                }
                            }

                            info!("------------------------------\n");
                        }
                        Message::Unchoke => {
                            self.session.state.peer_choking = false;
                            info!("---------------------------------");
                            info!("| {:?} Unchoke  |", self.addr);
                            info!("---------------------------------");

                            if self.can_request() {
                                self.session.prepare_for_download(self.extension.reqq);
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

                            let torrent_ctx = self.torrent_ctx.clone();
                            let torrent_p = torrent_ctx.pieces.read().await;
                            let bit_item = torrent_p.get(piece);

                            // maybe become interested in peer and request blocks
                            if !self.session.state.am_interested {
                                match bit_item {
                                    Some(a) => {
                                        if a.bit == 1 {
                                            info!("already have this piece, ignoring");
                                        } else {
                                            info!("We do not have this piece, sending interested");
                                            info!("{:?} we are interested due to Have", self.addr);

                                            self.session.state.am_interested = true;
                                            sink.send(Message::Interested).await?;
                                        }
                                    }
                                    None => {
                                        info!("We do not have `info` downloaded yet, sending interested");
                                        info!("{:?} we are interested due to Have", self.addr);
                                        self.session.state.am_interested = true;
                                        sink.send(Message::Interested).await?;
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

                            if self.can_request() {
                                self.handle_piece_msg(block).await?;
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

                                self.disk_tx.send(
                                    DiskMsg::ReadBlock {
                                        b: block_info,
                                        recipient: tx,
                                        info_hash: self.torrent_ctx.info_hash,
                                    }
                                )
                                .await?;

                                let bytes = rx.await??;

                                let block = Block {
                                    index,
                                    begin,
                                    block: bytes,
                                };
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

                                if let Ok(extension) = Extension::from_bencode(&payload) {
                                    info!("extension of peer: {:?}", extension);
                                    self.extension = extension;

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

                                            let (tx, rx) = oneshot::channel();
                                            self.torrent_ctx.tx.send(TorrentMsg::RequestInfoPiece(metadata.piece, tx)).await?;

                                            match rx.await? {
                                                Some(info_slice) => {
                                                    info!("sending data with piece {:?}", metadata.piece);
                                                    let r = Metadata::data(metadata.piece, &info_slice)?;
                                                    sink.send(
                                                        Message::Extended((ut_metadata, r))
                                                    )
                                                    .await?;
                                                }
                                                None => {
                                                    info!("sending reject");
                                                    let r = Metadata::reject(metadata.piece).to_bencode()
                                                        .map_err(|_| Error::BencodeError)?;
                                                    sink.send(
                                                        Message::Extended((ut_metadata, r))
                                                    )
                                                    .await?;
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

                                            self.torrent_ctx.tx.send(TorrentMsg::DownloadedInfoPiece(t, metadata.piece, info)).await?;
                                            // self.torrent_ctx.tx.send(TorrentMsg::SendCancelMetadata{
                                            //     from: self.ctx.id.read().await.unwrap(),
                                            //     index: metadata.piece
                                            // })
                                            // .await?;
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
                        PeerMsg::ReRequest(block_info) => {
                            if self.outgoing_requests.get(&block_info).is_some() && self.can_request() {
                                info!("{} rerequesting block_info due to timeout {block_info:?}", self.addr);

                                self.session.register_request_timeout();

                                if self.session.timed_out_request_count >= 3 {
                                    // only free timed out blocks?
                                    self.free_pending_block(block_info).await;
                                } else {
                                    sink.send(Message::Request(block_info.clone())).await?;
                                }
                            }
                        }
                        PeerMsg::HavePiece(piece) => {
                            info!("{:?} has piece {piece}", self.addr);

                            let pieces = self.ctx.pieces.read().await;

                            if let Some(b) = pieces.get(piece) {
                                // send Have to this peer if he doesnt have this piece
                                if b.bit == 0 {
                                    info!("sending have {piece} to peer {:?}", self.addr);
                                    let _ = sink.send(Message::Have(piece)).await;
                                }
                            }
                            else if pieces.len_bytes() == 0 {
                                let _ = sink.send(Message::Have(piece)).await;
                            }
                            drop(pieces);
                        }
                        PeerMsg::RequestBlockInfo(block_info) => {
                            info!("{:?} RequestBlockInfo {block_info:#?}", self.addr);

                            if self.outgoing_requests.len() < self.session.target_request_queue_len as usize && self.can_request() {
                                self.outgoing_requests.insert(block_info.clone());
                                self.request_block_infos(&mut sink).await?;
                            }
                        }
                        PeerMsg::RequestBlockInfos(block_infos) => {
                            info!("{:?} RequestBlockInfos len {}", self.addr, block_infos.len());

                            let max = self.session.target_request_queue_len as usize - self.outgoing_requests.len();

                            if self.can_request() {
                                self.session.last_outgoing_request_time = Some(std::time::Instant::now());

                                for block_info in block_infos.into_iter().take(max) {
                                    self.outgoing_requests.insert(block_info.clone());
                                    sink.send(Message::Request(block_info.clone())).await?;
                                }
                            }
                        }
                        PeerMsg::NotInterested => {
                            self.session.state.am_interested = false;
                            sink.send(Message::NotInterested).await?;
                        }
                        PeerMsg::Pause => {
                            self.session.state.prev_peer_choking = self.session.state.peer_choking;

                            if self.session.state.am_interested {
                                self.session.state.am_interested = false;
                                let _ = sink.send(Message::NotInterested).await;
                            }

                            if !self.session.state.peer_choking {
                                self.session.state.peer_choking = true;
                                let _ = sink.send(Message::Choke).await;
                            }

                            for block_info in &self.outgoing_requests {
                                sink.send(Message::Cancel(block_info.clone())).await?;
                            }

                            self.free_pending_blocks().await;
                        }
                        PeerMsg::Resume => {
                            self.session.state.peer_choking = self.session.state.prev_peer_choking;

                            if !self.session.state.peer_choking {
                                sink.send(Message::Unchoke).await?;
                            }

                            let p = self.ctx.pieces.read().await.clone();
                            for x in p {
                                if x.bit == 0 {
                                    info!("{:?} we are interested due to Bitfield", self.addr);

                                    self.session.state.am_interested = true;
                                    sink.send(Message::Interested).await?;

                                    if self.can_request() {
                                        self.request_block_infos(&mut sink).await?;
                                    }

                                    break;
                                }
                            }
                        }
                        PeerMsg::CancelBlock(block_info) => {
                            info!("{:?} sending Cancel", self.addr);
                            self.outgoing_requests.remove(&block_info);
                            sink.send(Message::Cancel(block_info)).await?;
                        }
                        PeerMsg::CancelMetadata(index) => {
                            info!("{:?} sending CancelMetadata", self.addr);
                            let metadata_reject = Metadata::reject(index);
                            let metadata_reject = metadata_reject.to_bencode().unwrap();

                            sink.send(Message::Extended((3, metadata_reject))).await?;
                        }
                        PeerMsg::Quit => {
                            info!("{:?} quitting", self.addr);
                            self.session.state.connection = ConnectionState::Quitting;
                            return Ok(());
                        }
                        PeerMsg::HaveInfo => {
                            debug!("{:?} has entire info", self.addr);
                            self.have_info = true;
                            let am_interested = self.session.state.am_interested;
                            let peer_choking = self.session.state.peer_choking;

                            if am_interested && !peer_choking {
                                self.session.prepare_for_download(self.extension.reqq);
                                debug!("{:?} requesting blocks", self.addr);
                                self.request_block_infos(&mut sink).await?;
                            }
                        }
                    }
                }
            }
        }
    }
    pub async fn handle_piece_msg(&mut self, block: Block) -> Result<(), Error> {
        let index = block.index;
        let begin = block.begin;
        let len = block.block.len();

        let block_info = BlockInfo {
            index: index as u32,
            begin,
            len: len as u32,
        };

        // remove pending block request
        let _existed = self.outgoing_requests.remove(&block_info);

        // if in endgame, send cancels to all other peers
        if self.session.in_endgame {
            let from = self.ctx.id.read().await.unwrap();
            let _ = self
                .torrent_ctx
                .tx
                .send(TorrentMsg::SendCancelBlock {
                    from,
                    block_info: block_info.clone(),
                })
                .await;
        }

        let (tx, rx) = oneshot::channel();

        self.disk_tx
            .send(DiskMsg::WriteBlock {
                b: block,
                recipient: tx,
                info_hash: self.torrent_ctx.info_hash,
            })
            .await?;

        rx.await??;

        // update stats
        self.session.update_download_stats(len as u32);

        Ok(())
    }
    pub async fn tick<T>(&mut self, sink: &mut T) -> Result<(), Error>
    where
        T: SinkExt<Message> + Sized + std::marker::Unpin,
    {
        debug!("tick peer");

        if self.can_request() {
            self.request_block_infos(sink).await?;
        }

        // resend requests if we have pending requests and more time has elapsed
        // since the last request than the current timeout value
        if !self.outgoing_requests.is_empty() {
            self.check_request_timeout(sink).await?;
        }

        self.session.counters.reset();

        Ok(())
    }

    /// Times out the peer if it hasn't sent a request in too long.
    async fn check_request_timeout<T>(&mut self, sink: &mut T) -> Result<(), Error>
    where
        T: SinkExt<Message> + Sized + std::marker::Unpin,
    {
        info!(
            "{:?} check_request_timeout outgoing_requests {}",
            self.addr,
            self.outgoing_requests.len()
        );
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
                    "{:?} timeout after {} ms, freeing {} request(s) (timeouts: {})",
                    self.addr,
                    elapsed_since_last_request.as_millis(),
                    self.outgoing_requests.len(),
                    self.session.timed_out_request_count + 1
                );

                warn!("inflight blocks {}", self.outgoing_requests.len());
                warn!("can request {}", self.session.target_request_queue_len);

                // todo: only free blocks that timed out?
                self.free_pending_blocks().await;
                self.session.register_request_timeout();
                if self.can_request() {
                    self.request_block_infos(sink).await?;
                }
            }
        }

        Ok(())
    }

    pub async fn free_pending_block(&mut self, block_info: BlockInfo) {
        info!(
            "{:?} freeing {:?} block for download",
            self.addr, block_info
        );

        self.session.timed_out_request_count = 0;
        self.session.request_timed_out = false;

        let _ = self
            .disk_tx
            .send(DiskMsg::ReturnBlockInfos(
                self.torrent_ctx.info_hash,
                VecDeque::from(vec![block_info]),
            ))
            .await;
    }

    /// Marks requested blocks as free in their respective downlaods so that
    /// other peer sessions may download them.
    pub async fn free_pending_blocks(&mut self) {
        let blocks: VecDeque<BlockInfo> = self.outgoing_requests.drain().collect();

        info!(
            "{:?} freeing {:?} blocks for download",
            self.addr,
            blocks.len()
        );

        // The piece may no longer be present if it was completed by
        // another peer in the meantime and torrent removed it from the
        // shared download store. This is fine, in this case we don't have
        // anything to do.

        // send this block_info back to the vec of available block_infos,
        // so that other peers can download it.
        if !blocks.is_empty() {
            let _ = self
                .disk_tx
                .send(DiskMsg::ReturnBlockInfos(
                    self.torrent_ctx.info_hash,
                    blocks,
                ))
                .await;
        }
    }

    #[tracing::instrument(skip(self, sink))]
    pub async fn request_block_infos<T>(&mut self, sink: &mut T) -> Result<(), Error>
    where
        T: SinkExt<Message> + Sized + std::marker::Unpin,
    {
        // on the first request, we only ask for 1 block,
        // the first peer to answer is probably the fastest peer.
        // when we receive the block, we request the max amount.
        // this is useful in the scenario of downloading a small torrent,
        // the first peer can easily request all blocks of the torrent.
        // if the peer is a slow one, we wasted a lot of time.
        let target_request_queue_len = self.session.target_request_queue_len as usize;

        // the number of blocks we can request right now
        let request_len = if self.outgoing_requests.len() >= target_request_queue_len {
            0
        } else {
            target_request_queue_len - self.outgoing_requests.len()
        };

        if request_len > 0 {
            info!("inflight: {}", self.outgoing_requests.len());
            info!("max to request: {}", target_request_queue_len);
            info!("requesting len: {request_len}");

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

            let r = orx.await?;

            if r.is_empty() && !self.session.in_endgame && self.outgoing_requests.len() <= 20 {
                // endgame is probably slowing down the download_rate for some reason?
                self.start_endgame().await;
            }

            self.session.last_outgoing_request_time = Some(std::time::Instant::now());

            for block_info in r {
                info!("{:?} requesting {block_info:#?}", self.addr);

                self.outgoing_requests.insert(block_info.clone());

                let _ = sink.send(Message::Request(block_info.clone())).await;

                let req_id: u64 = MessageId::Request as u64;

                let _timeout = self.session.request_timeout();
                let _tx = self.ctx.tx.clone();

                // spawn(async move {
                //     info!("rerequest timeout is {timeout:?}");
                //     tokio::time::sleep(timeout).await;
                //     let _ = tx.send(PeerMsg::ReRequest(block_info)).await;
                // });

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
            let outgoing: Vec<BlockInfo> = self.outgoing_requests.drain().collect();
            let _ = self
                .torrent_ctx
                .tx
                .send(TorrentMsg::StartEndgame(id, outgoing))
                .await;
        }
    }

    #[tracing::instrument(skip(self, sink))]
    pub async fn maybe_request_info<T>(&mut self, sink: &mut T) -> Result<(), Error>
    where
        T: SinkExt<Message> + Sized + std::marker::Unpin,
    {
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
