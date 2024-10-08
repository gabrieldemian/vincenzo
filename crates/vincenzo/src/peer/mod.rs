//! A remote peer in the network that downloads and uploads data
pub mod session;
use bendy::encoding::ToBencode;
use bitvec::{
    bitvec,
    prelude::{BitArray, Msb0},
};
use futures::{SinkExt, StreamExt};
use hashbrown::{HashMap, HashSet};
use std::{collections::VecDeque, net::SocketAddr, sync::Arc, time::Duration};
use tokio::{
    select,
    sync::{
        mpsc::{self, Receiver},
        oneshot, RwLock,
    },
    time::{interval, interval_at, Instant},
};
use tokio_util::codec::{Framed, FramedParts};

use tokio::net::TcpStream;
use tracing::{debug, warn};

use crate::extensions::{
    core::MessageCodec,
    extended::{ExtensionTrait, MessageTrait},
};

use crate::{
    bitfield::{Bitfield, Reserved},
    disk::DiskMsg,
    error::Error,
    extensions::{
        core::{
            Block, BlockInfo, Core, CoreId, Handshake, HandshakeCodec, Message,
            BLOCK_LEN,
        },
        extended::Extension,
        metadata::Metadata,
    },
    peer::session::ConnectionState,
    torrent::{TorrentCtx, TorrentMsg},
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

/// Cores that peers send to each other.
#[derive(Debug)]
pub enum PeerMsg {
    /// When we download a full piece, we need to send Have's
    /// to peers that dont Have it.
    HavePiece(usize),
    /// Sometimes a peer either takes too long to answer,
    /// or simply does not answer at all. In both cases
    /// we need to request the block again.
    RequestBlockInfos(Vec<BlockInfo>),
    /// Tell this peer that we are not interested,
    /// update the local state and send a message to the peer
    NotInterested,
    /// Sends a Cancel message to cancel a block info that we
    /// expect the peer to send us, because we requested it previously.
    CancelBlock(BlockInfo),
    /// Sends a Cancel message to cancel a metadata piece that we
    /// expect the peer to send us, because we requested it previously.
    CancelMetadata(u32),
    /// Sent when the torrent has downloaded the entire info of the torrent.
    HaveInfo,
    /// Sent when the torrent is paused, it makes the peer pause downloads and
    /// uploads
    Pause,
    /// Sent when the torrent was unpaused.
    Resume,
    /// Sent to make this peer read-only, the peer won't download
    /// anymore, but it will still seed.
    /// This usually happens when the torrent is fully downloaded.
    SeedOnly,
    /// When the program is being gracefuly shutdown, we need to kill the tokio
    /// green thread of the peer.
    Quit,
}

/// Data about a remote Peer that the client is connected to,
/// but the client itself does not have a Peer struct.
#[derive(Debug)]
pub struct Peer {
    /// Extensions of the protocol that the peer supports.
    pub extension: Extension,
    pub reserved: BitArray<[u8; 8], Msb0>,
    pub torrent_ctx: Arc<TorrentCtx>,
    pub rx: Receiver<PeerMsg>,
    /// Context of the Peer which is shared for anyone who needs it.
    pub ctx: Arc<PeerCtx>,
    /// Most of the session's information and state is stored here, i.e. it's
    /// the "context" of the session, with information like: endgame mode, slow
    /// start, download_rate, etc.
    pub session: Session,
    /// Our pending requests that we sent to peer. It represents the blocks
    /// that we are expecting.
    ///
    /// If we receive a block whose request entry is here, that entry is
    /// removed. A request is also removed here when it is timed out.
    pub outgoing_requests: HashSet<BlockInfo>,
    /// The Instant of each timeout value of [`Self::outgoing_requests`]
    /// blocks.
    pub outgoing_requests_timeout: HashMap<BlockInfo, Instant>,
    /// The requests we got from peer.
    ///
    /// The request's entry is removed from here when the block is transmitted
    /// or when the peer cancels it. If a peer sends a request and cancels it
    /// before the disk read is done, the read block is dropped.
    pub incoming_requests: HashSet<BlockInfo>,
    /// This is a cache of have_info on Torrent
    /// to avoid using locks or atomics.
    pub have_info: bool,
}

#[derive(Debug, Clone)]
pub struct PeerId([u8; 20]);

impl TryInto<PeerId> for String {
    type Error = String;
    fn try_into(self) -> Result<PeerId, Self::Error> {
        let buff = hex::decode(self).map_err(|e| e.to_string())?;
        let hash = PeerId::try_from(buff)?;
        Ok(hash)
    }
}

impl Into<String> for PeerId {
    fn into(self) -> String {
        hex::encode(self.0)
    }
}

impl From<[u8; 20]> for PeerId {
    fn from(value: [u8; 20]) -> Self {
        Self(value)
    }
}

impl TryFrom<Vec<u8>> for PeerId {
    type Error = &'static str;
    fn try_from(value: Vec<u8>) -> Result<Self, Self::Error> {
        if value.len() != 20 {
            return Err("The PeerId must have exactly 20 bytes");
        }
        let mut buff = [0u8; 20];
        buff[..20].copy_from_slice(&value[..20]);
        Ok(PeerId(buff))
    }
}

/// Ctx that is shared with Torrent and Disk;
#[derive(Debug)]
pub struct PeerCtx {
    pub direction: Direction,
    pub tx: mpsc::Sender<PeerMsg>,
    /// a `Bitfield` with pieces that this peer
    /// has, and hasn't, containing 0s and 1s
    pub pieces: RwLock<Bitfield>,
    /// Updated when the peer sends us its peer
    /// id, in the handshake.
    pub id: [u8; 20],
    /// Where the TCP socket of this Peer is connected to.
    pub remote_addr: SocketAddr,
    /// Where the TCP socket of this Peer is listening.
    pub local_addr: SocketAddr,
    /// The info_hash of the torrent that this Peer belongs to.
    pub info_hash: [u8; 20],
}

impl Peer {
    /// Do a handshake with a remote peer and return the remote peer's
    /// handshake.
    ///
    /// # Important
    /// A peer cannot exist on it's own, it needs to be handshaked with another
    /// peer in order to "exist". Only in this moment it gains it's peer_id
    /// and other data.
    ///
    /// The right order to create and run a Peer is the following:
    /// handshake -> new -> run
    pub async fn handshake(
        socket: TcpStream,
        direction: Direction,
        info_hash: [u8; 20],
        local_peer_id: [u8; 20],
    ) -> Result<(Framed<TcpStream, MessageCodec>, Handshake), Error>
// where
    //     M: Into<Message> + From<Core>,
    //     C: Encoder<M> + Decoder + Unpin,
    {
        let local = socket.local_addr()?;
        let remote = socket.peer_addr()?;
        let mut socket = Framed::new(socket, HandshakeCodec);
        let our_handshake = Handshake::new(info_hash, local_peer_id);
        let peer_handshake: Handshake;

        // we are connecting, send the first handshake
        if direction == Direction::Outbound {
            debug!("{local} sending the first handshake to {remote}");
            socket.send(our_handshake.clone()).await?;
        }

        // wait for, and validate, their handshake
        if let Some(Ok(their_handshake)) = socket.next().await {
            debug!("{local} received their handshake {remote}");
            peer_handshake = their_handshake.clone();

            if !their_handshake.validate(&our_handshake) {
                return Err(Error::HandshakeInvalid);
            }

            // receive the second handshake, if outbound
            if direction == Direction::Outbound {
                let old_parts = socket.into_parts();
                let mut new_parts =
                    FramedParts::new(old_parts.io, MessageCodec);
                new_parts.read_buf = old_parts.read_buf;
                new_parts.write_buf = old_parts.write_buf;
                let socket = Framed::from_parts(new_parts);
                return Ok((socket, peer_handshake));
            }
        } else {
            warn!("{remote} did not send a handshake");
            return Err(Error::HandshakeInvalid);
        }

        // if inbound, he have already received their first handshake,
        // send our second handshake here.
        if direction == Direction::Inbound {
            debug!("{local} sending the second handshake to {remote}");
            socket.send(our_handshake.clone()).await?;
        }

        let old_parts = socket.into_parts();
        let mut new_parts = FramedParts::new(old_parts.io, MessageCodec);
        new_parts.read_buf = old_parts.read_buf;
        new_parts.write_buf = old_parts.write_buf;
        let socket = Framed::from_parts(new_parts);

        Ok((socket, peer_handshake))
    }

    /// Create a new [`Peer`] given it's handshake.
    ///
    /// # Important
    /// This MUST be called AFTER the method `handshake`.
    pub fn new(
        direction: Direction,
        remote_addr: SocketAddr,
        torrent_ctx: Arc<TorrentCtx>,
        handshake: Handshake,
        local_addr: SocketAddr,
    ) -> Self {
        let (tx, rx) = mpsc::channel::<PeerMsg>(300);

        let ctx = Arc::new(PeerCtx {
            direction,
            remote_addr,
            pieces: RwLock::new(Bitfield::new()),
            id: handshake.peer_id,
            tx,
            info_hash: handshake.info_hash,
            local_addr,
        });

        let reserved = Reserved::from(handshake.reserved);

        Peer {
            incoming_requests: HashSet::default(),
            outgoing_requests: HashSet::default(),
            outgoing_requests_timeout: HashMap::new(),
            session: Session::default(),
            have_info: false,
            extension: Extension::default(),
            reserved,
            torrent_ctx,
            ctx,
            rx,
        }
    }

    /// Start the event loop of the Peer, listen to messages sent by others
    /// on the peer wire protocol.
    #[tracing::instrument(skip_all, name = "peer::run")]
    pub async fn run(
        &mut self,
        direction: Direction,
        mut socket: Framed<TcpStream, MessageCodec>,
    ) -> Result<(), Error>
// where
    //     M: Into<Message> + From<Core>,
    //     C: Encoder<M> + Decoder + Unpin + ExtensionTrait,
    {
        self.session.state.connection = ConnectionState::Connecting;
        let local = self.ctx.local_addr;
        let remote = self.ctx.remote_addr;

        // if they are connecting, answer with our extended handshake
        // if supported
        if direction == Direction::Inbound {
            // The bit selected for the extension protocol is bit 20 from the
            // right and bit 44 from the left
            if self.reserved[43] {
                debug!("{local} sending extended handshake to {remote}");

                // we need to have the info downloaded in order to send the
                // extended message, because it contains the metadata_size
                if self.have_info {
                    let info = self.torrent_ctx.info.read().await;
                    let metadata_size = info
                        .to_bencode()
                        .map_err(|_| Error::BencodeError)?
                        .len();
                    drop(info);

                    let ext = Extension::supported(Some(metadata_size as u32))
                        .to_bencode()
                        .map_err(|_| Error::BencodeError)?;

                    let extended = Core::Extended(0, ext);

                    socket.send(extended.into()).await?;
                    self.try_request_info(&mut socket).await?;
                }
            }
        }

        let _ = self
            .torrent_ctx
            .tx
            .send(TorrentMsg::PeerConnected(self.ctx.id, self.ctx.clone()))
            .await;

        let mut tick_timer = interval(Duration::from_secs(1));

        let mut keep_alive_timer = interval_at(
            Instant::now() + Duration::from_secs(120),
            Duration::from_secs(120),
        );

        let (mut sink, mut stream) = socket.split();

        // maybe send bitfield
        let bitfield = self.torrent_ctx.bitfield.read().await;
        if bitfield.len() > 0 {
            debug!("{local} sending bitfield to {remote}");
            sink.send(Core::Bitfield(bitfield.clone()).into()).await?;
        }
        drop(bitfield);

        // todo: implement choke algorithm
        // send Unchoke
        self.session.state.am_choking = false;
        sink.send(Core::Unchoke.into()).await?;
        // sink.send(Core::Interested).await?;

        // when running a new Peer, we might
        // already have the info downloaded.
        let have = self
            .torrent_ctx
            .has_at_least_one_piece
            .load(std::sync::atomic::Ordering::Relaxed);

        if have {
            self.have_info = have;
        }

        let info = self.torrent_ctx.info.read().await;
        let mut peer_pieces = self.ctx.pieces.write().await;

        // if local peer has info, initialize the bitfield
        // of this peer.
        if peer_pieces.is_empty() && info.piece_length != 0 {
            *peer_pieces = bitvec![u8, Msb0; 0; info.pieces() as usize];
        }

        drop(peer_pieces);
        drop(info);

        loop {
            select! {
                // update internal data every 1 second
                _ = tick_timer.tick(), if self.have_info => {
                    self.tick(&mut sink).await?;
                }
                // send Keepalive every 2 minutes
                _ = keep_alive_timer.tick(), if self.have_info => {
                    sink.send(Core::KeepAlive.into()).await?;
                }
                Some(Ok(msg)) = stream.next() => {
                    msg.handle_msg(self, &mut sink).await?;
                    // match msg {
                    //     Message::CoreCodec(m) => {
                    //         // let c = m.codec();
                    //         // if c.is_supported(&self.extension) {
                    //         //     c.handle_msg(&m, self, &mut sink).await?;
                    //         // }
                    //     }
                    //     _ => todo!()
                    // }
                }
                Some(msg) = self.rx.recv() => {
                    match msg {
                        PeerMsg::HavePiece(piece) => {
                            debug!("{local} has piece {piece}");

                            self.have_info = true;
                            let pieces = self.ctx.pieces.read().await;

                            if let Some(b) = pieces.get(piece) {
                                // send Have to this peer if he doesnt have this piece
                                if !b {
                                    debug!("sending have {piece} to peer {local}");
                                    let _ = sink.send(Core::Have(piece).into()).await;
                                }
                            }
                            drop(pieces);
                        }
                        PeerMsg::RequestBlockInfos(block_infos) => {
                            debug!("{local} RequestBlockInfos len {}", block_infos.len());

                            let max = self.session.target_request_queue_len as usize - self.outgoing_requests.len();

                            if self.can_request() {
                                self.session.last_outgoing_request_time = Some(Instant::now());

                                for block_info in block_infos.into_iter().take(max) {
                                    self.outgoing_requests.insert(
                                        block_info.clone()
                                    );

                                    self.outgoing_requests_timeout
                                        .insert(block_info.clone(), Instant::now());

                                    sink.send(Core::Request(block_info).into()).await?;
                                }
                            }
                        }
                        PeerMsg::NotInterested => {
                            debug!("{local} NotInterested {remote}");
                            self.session.state.am_interested = false;
                            sink.send(Core::NotInterested.into()).await?;
                        }
                        PeerMsg::Pause => {
                            debug!("{local} Pause");
                            self.session.state.prev_peer_choking = self.session.state.peer_choking;

                            if self.session.state.am_interested {
                                self.session.state.am_interested = false;
                                let _ = sink.send(Core::NotInterested.into()).await;
                            }

                            if !self.session.state.peer_choking {
                                self.session.state.peer_choking = true;
                                let _ = sink.send(Core::Choke.into()).await;
                            }

                            for block_info in &self.outgoing_requests {
                                sink.send(Core::Cancel(block_info.clone()).into()).await?;
                            }

                            self.free_pending_blocks().await;
                        }
                        PeerMsg::Resume => {
                            debug!("{local} Resume");
                            self.session.state.peer_choking = self.session.state.prev_peer_choking;

                            if !self.session.state.peer_choking {
                                sink.send(Core::Unchoke.into()).await?;
                            }

                            let peer_has_piece = self.has_piece_not_in_local().await;

                            if peer_has_piece {
                                debug!("{local} we are interested due to Bitfield");
                                self.session.state.am_interested = true;
                                sink.send(Core::Interested.into()).await?;

                                if self.can_request() {
                                    self.request_block_infos(&mut sink).await?;
                                }
                            }
                        }
                        PeerMsg::CancelBlock(block_info) => {
                            debug!("{local} CancelBlock {remote}");
                            self.outgoing_requests.remove(&block_info);
                            self.outgoing_requests_timeout.remove(&block_info);
                            sink.send(Core::Cancel(block_info).into()).await?;
                        }
                        PeerMsg::SeedOnly => {
                            debug!("{local} SeedOnly");
                            self.session.seed_only = true;
                        }
                        PeerMsg::CancelMetadata(index) => {
                            debug!("{local} CancelMetadata {remote}");
                            let metadata_reject = Metadata::reject(index);
                            let metadata_reject = metadata_reject.to_bencode().unwrap();

                            sink.send(Core::Extended(3, metadata_reject).into()).await?;
                        }
                        PeerMsg::Quit => {
                            debug!("{local} Quit");
                            self.session.state.connection = ConnectionState::Quitting;
                            return Ok(());
                        }
                        PeerMsg::HaveInfo => {
                            debug!("{local} HaveInfo");
                            self.have_info = true;
                            let am_interested = self.session.state.am_interested;
                            let peer_choking = self.session.state.peer_choking;

                            debug!("{local} am_interested {am_interested}");
                            debug!("{local} peer_choking {peer_choking}");

                            if am_interested && !peer_choking {
                                self.prepare_for_download().await;
                                debug!("{local} requesting blocks");
                                self.request_block_infos(&mut sink).await?;
                            }
                        }
                    }
                }
            }
        }
    }

    /// Check if we can request new blocks, if:
    /// - We are not being choked by the peer
    /// - We are interested in the peer
    /// - We have the downloaded the info of the torrent
    /// - The torrent is not fully downloaded (peer is not in seed-only mode)
    /// - The capacity of inflight blocks is not full (len of outgoing_requests)
    pub fn can_request(&self) -> bool {
        let am_interested = self.session.state.am_interested;
        let am_choking = self.session.state.am_choking;
        let have_capacity = self.outgoing_requests.len()
            < self.session.target_request_queue_len as usize;

        am_interested
            && !am_choking
            && self.have_info
            && have_capacity
            && !self.session.seed_only
    }

    /// Handle a new Piece msg from the peer, a Piece msg actually sends
    /// a block, and not a piece.
    pub async fn handle_piece_msg(
        &mut self,
        block: Block,
    ) -> Result<(), Error> {
        let index = block.index;
        let begin = block.begin;
        let len = block.block.len();

        let block_info =
            BlockInfo { index: index as u32, begin, len: len as u32 };

        // remove pending block request
        self.outgoing_requests.remove(&block_info);
        self.outgoing_requests_timeout.remove(&block_info);

        // if in endgame, send cancels to all other peers
        if self.session.in_endgame {
            let from = self.ctx.id;
            let _ = self
                .torrent_ctx
                .tx
                .send(TorrentMsg::SendCancelBlock {
                    from,
                    block_info: block_info.clone(),
                })
                .await;
        }

        // let (tx, rx) = oneshot::channel();

        self.torrent_ctx
            .disk_tx
            .send(DiskMsg::WriteBlock {
                block,
                // recipient: tx,
                info_hash: self.torrent_ctx.info_hash,
            })
            .await?;

        // rx.await??;

        // update stats
        self.session.update_download_stats(len as u32);

        Ok(())
    }

    /// Periodic tick of the [`Peer`]. This function must be called every 1
    /// seconds to:
    /// - Check if we can request blocks.
    /// - Check and resend requests that timed out.
    /// - Update stats about the Peer.
    pub async fn tick<M, C>(&mut self, sink: &mut C) -> Result<(), Error>
    where
        M: From<Core> + Into<Message>,
        C: SinkExt<M> + Sized + std::marker::Unpin,
    {
        // resend requests if we have any pending and more time has elapsed
        // since the last received block than the current timeout value
        if !self.outgoing_requests.is_empty() && self.can_request() {
            self.check_request_timeout(sink).await?;
        }

        self.session.counters.reset();

        Ok(())
    }

    /// Re-request blocks that timed-out
    async fn check_request_timeout<T, M>(
        &mut self,
        sink: &mut T,
    ) -> Result<(), Error>
    where
        M: Into<Message> + From<Core>,
        T: SinkExt<M> + Sized + std::marker::Unpin,
    {
        let local = self.ctx.local_addr;

        // if self.session.timed_out_request_count >= 10 {
        //     self.free_pending_blocks().await;
        //     return Ok(());
        // }

        for (block, timeout) in self.outgoing_requests_timeout.iter_mut() {
            let elapsed_since_last_request =
                Instant::now().saturating_duration_since(*timeout);

            // if the timeout time has already passed,
            if elapsed_since_last_request >= self.session.request_timeout() {
                self.session.register_request_timeout();

                debug!(
                    "{local} this block {block:#?} timed out \
                    ({} ms ago)",
                    elapsed_since_last_request.as_millis(),
                );

                let _ = sink.send(Core::Request(block.clone()).into()).await;
                *timeout = Instant::now();

                debug!(
                    "{local} timeout, total: {}",
                    self.session.timed_out_request_count + 1
                );
            }
        }
        if !self.session.seed_only {
            self.request_block_infos(sink).await?;
        }

        Ok(())
    }

    /// Take the block infos that are in queue and send them back
    /// to the disk so that other peers can request those blocks.
    /// A good example to use this is when the Peer is no longer
    /// available (disconnected).
    pub async fn free_pending_blocks(&mut self) {
        let local = self.ctx.local_addr;
        let remote = self.ctx.remote_addr;
        let blocks: VecDeque<BlockInfo> =
            self.outgoing_requests.drain().collect();
        self.outgoing_requests_timeout.clear();

        self.session.timed_out_request_count = 0;

        debug!(
            "{local} freeing {:?} blocks for download of {remote}",
            blocks.len()
        );

        // send this block_info back to the vec of available block_infos,
        // so that other peers can download it.
        if !blocks.is_empty() {
            let _ = self
                .torrent_ctx
                .disk_tx
                .send(DiskMsg::ReturnBlockInfos(
                    self.torrent_ctx.info_hash,
                    blocks,
                ))
                .await;
        }
    }

    /// Request new block infos to this Peer's remote address.
    /// Must be used after checking that the Peer is able to send blocks with
    /// [`Self::can_request`].
    #[tracing::instrument(level="debug", skip_all, fields(self.local_addr))]
    pub async fn request_block_infos<T, M>(
        &mut self,
        sink: &mut T,
    ) -> Result<(), Error>
    where
        M: Into<Message> + From<Core>,
        T: SinkExt<M> + Sized + std::marker::Unpin,
    {
        if self.session.seed_only {
            warn!("Calling request_block_infos when peer is in seed-only mode");
            return Ok(());
        }
        let local = self.ctx.local_addr;
        // let remote = self.ctx.remote_addr;

        let target_request_queue_len =
            self.session.target_request_queue_len as usize;

        // the number of blocks we can request right now
        let request_len =
            if self.outgoing_requests.len() >= target_request_queue_len {
                0
            } else {
                target_request_queue_len - self.outgoing_requests.len()
            };

        debug!("inflight: {}", self.outgoing_requests.len());
        debug!("max to request: {}", target_request_queue_len);
        debug!("request_len: {request_len}");
        debug!("target_request_queue_len: {target_request_queue_len}");

        if request_len > 0 {
            debug!("{local} peer requesting l: {:?} block infos", request_len);
            // get a list of unique block_infos from the Disk,
            // those are already marked as requested on Torrent
            let (otx, orx) = oneshot::channel();
            let _ = self
                .torrent_ctx
                .disk_tx
                .send(DiskMsg::RequestBlocks {
                    recipient: otx,
                    qnt: request_len,
                    info_hash: self.torrent_ctx.info_hash,
                    peer_id: self.ctx.id,
                })
                .await;

            let r = orx.await?;

            let f = r.front();
            debug!("first block requested {f:?}");

            if r.is_empty()
                && !self.session.in_endgame
                && self.outgoing_requests.len() <= 20
            {
                // self.start_endgame().await;
            }

            debug!("disk sent {:?} blocks", r.len());
            self.session.last_outgoing_request_time = Some(Instant::now());

            for block_info in r {
                // debug!("{local} requesting \n {block_info:#?} to {remote}");
                self.outgoing_requests.insert(block_info.clone());

                let _ =
                    sink.send(Core::Request(block_info.clone()).into()).await;

                self.outgoing_requests_timeout
                    .insert(block_info, Instant::now());

                let req_id: u64 = CoreId::Request as u64;

                self.session.counters.protocol.up += req_id;
            }
        } else {
            debug!("{local} no more blocks to request");
        }

        Ok(())
    }

    /// Start endgame mode. This will take the few remaining block infos
    /// and request them to all the peers of the torrent. After the first peer
    /// receives it, it send Cancel messages to all other peers.
    #[tracing::instrument(skip(self))]
    pub async fn start_endgame(&mut self) {
        self.session.in_endgame = true;

        let outgoing: Vec<BlockInfo> = self.outgoing_requests.drain().collect();
        let _ = self
            .torrent_ctx
            .tx
            .send(TorrentMsg::StartEndgame(self.ctx.id, outgoing))
            .await;
    }

    /// Maybe request an info piece from this Peer if:
    /// - The peer supports the "ut_metadata" extension from the extension
    ///   protocol
    /// - We do not have the info downloaded
    #[tracing::instrument(skip(self, sink))]
    pub async fn try_request_info<T, M>(
        &mut self,
        sink: &mut T,
    ) -> Result<(), Error>
    where
        M: Into<Message> + From<Core>,
        T: SinkExt<M> + Sized + std::marker::Unpin,
    {
        // only request info if we dont have an Info
        // and the peer supports the metadata extension protocol
        if !self.have_info {
            // send bep09 request to get the Info
            if let Some(ut_metadata) = self.extension.m.ut_metadata {
                debug!(
                    "peer supports ut_metadata {ut_metadata}, sending request"
                );

                let t = self.extension.metadata_size.unwrap();
                let pieces = t as f32 / BLOCK_LEN as f32;
                let pieces = pieces.ceil() as u32;
                debug!("this info has {pieces} pieces");

                for i in 0..pieces {
                    let h = Metadata::request(i);

                    debug!("requesting info piece {i}");
                    debug!("request {h:?}");

                    let h = h.to_bencode().map_err(|_| Error::BencodeError)?;
                    let _ =
                        sink.send(Core::Extended(ut_metadata, h).into()).await;
                }
            }
        }
        Ok(())
    }

    /// If this Peer has a piece that the local Peer (client)
    /// does not have.
    pub async fn has_piece_not_in_local(&self) -> bool {
        // bitfield of the peer
        let bitfield = self.ctx.pieces.read().await;

        // local bitfield of the local peer
        let local_bitfield = self.torrent_ctx.bitfield.read().await;

        // when we don't have the info fully downloaded yet,
        // and the peer has already sent a bitfield or a have.
        if local_bitfield.is_empty() {
            return true;
        }

        for (local_piece, piece) in local_bitfield.iter().zip(bitfield.iter()) {
            if *piece && !local_piece {
                return true;
            }
        }
        false
    }

    /// Calculate the maximum number of block infos to request,
    /// and set this value on `session` of the peer.
    pub async fn prepare_for_download(&mut self) {
        debug_assert!(self.session.state.am_interested);
        debug_assert!(!self.session.state.am_choking);

        let has_one_piece = self
            .torrent_ctx
            .has_at_least_one_piece
            .load(std::sync::atomic::Ordering::Relaxed);

        // the max number of block_infos to request
        let n = if has_one_piece {
            debug!("has one piece, changing it to {:?}", self.extension.reqq);
            self.extension.reqq.unwrap_or(Session::DEFAULT_REQUEST_QUEUE_LEN)
        } else {
            // self.torrent_ctx.info.read().await.pieces() as u16
            self.extension.reqq.unwrap_or(Session::DEFAULT_REQUEST_QUEUE_LEN)
        };

        if n > 0 {
            self.session.target_request_queue_len = n;
        }
    }
}
