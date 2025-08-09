//! A remote peer in the network that downloads and uploads data
mod types;

use bendy::encoding::ToBencode;
// re-exports
pub use types::*;

use futures::{SinkExt, StreamExt};
use std::{sync::atomic::Ordering, time::Duration};
use tokio::{
    select,
    sync::oneshot,
    time::{interval, interval_at, Instant},
};

use tracing::{debug, info};

use crate::extensions::{
    ExtMsg, ExtMsgHandler, Extended, ExtendedMessage, Metadata,
};

use crate::{
    disk::DiskMsg,
    error::Error,
    extensions::core::{Block, BlockInfo, Core, BLOCK_LEN},
    torrent::TorrentMsg,
};

/// Data about a remote Peer that the client is connected to,
/// but the client itself does not have a Peer struct.
#[derive(Default)]
pub struct Peer<S: PeerState> {
    pub state: S,
    pub(crate) state_log: StateLog,
}

/// Handle peer messages.
/// Each extension will use this type to implement a trait to handle messages of
/// its extension.
pub struct MsgHandler;

pub static BLOCK_TIMEOUT: Duration = Duration::from_secs(3);

impl Peer<Connected> {
    /// Start the event loop of the Peer, listen to messages sent by others
    /// on the peer wire protocol.
    #[tracing::instrument(name = "peer", skip_all,
        fields(
            state = %self.state_log,
            addr = %self.state.ctx.remote_addr,
        )
    )]
    pub async fn run(&mut self) -> Result<(), Error> {
        self.state.connection = ConnectionState::Connecting;

        let _ = self
            .state
            .torrent_ctx
            .tx
            .send(TorrentMsg::PeerConnected(self.state.ctx.clone()))
            .await;

        // request info
        let mut info_interval = interval(Duration::from_secs(1));

        // request block infos
        let mut request_interval = interval(Duration::from_millis(500));

        // rerequest timedout blocks
        let mut rerequest_timeout_interval = interval(Duration::from_secs(3));

        // send interested or uninterested.
        // algorithm:
        // - 1. if peers contain at least 1 piece which we don't have, send
        //   interested.
        // - 2. later, if we already have all pieces which the peer has, and we
        //   are interested, send not interested.
        let mut interested_interval = interval(Duration::from_secs(3));

        // send message to keep the connection alive
        let mut keep_alive_interval = interval_at(
            Instant::now() + Duration::from_secs(60),
            Duration::from_secs(60),
        );

        // send bitfield
        {
            let (otx, orx) = oneshot::channel();
            let _ = self
                .state
                .torrent_ctx
                .tx
                .send(TorrentMsg::ReadBitfield(otx))
                .await;

            let bitfield = orx.await?;

            debug!("sending bitfield");

            if !bitfield.is_empty() {
                self.state.sink.send(Core::Bitfield(bitfield)).await?;
            }
        }

        // when running a new Peer, we might
        // already have the info downloaded.
        {
            let info = self.state.torrent_ctx.info.read().await;
            self.state.have_info = info.piece_length > 0;
        }

        self.state.connection = ConnectionState::Connected;

        loop {
            select! {
                // try to rerequest timedout meta info requests,
                // and request new ones if the peer can accept more.
                _ = info_interval.tick(), if !self.state.have_info => {
                    let Some(ut_metadata) = self
                        .state
                        .ext_states
                        .extension
                        .as_ref()
                        .and_then(|v| v.m.ut_metadata)
                    else {
                        continue;
                    };

                    self.try_request_info().await?;

                    // only re-request timed-out pieces if we have some
                    if self.state.outgoing_requests_info_pieces.is_empty() { continue };

                    let now = Instant::now();

                    // Check for timed-out requests (10 seconds)
                    for (piece, mut request_time) in &mut self.state.outgoing_requests_info_pieces {
                        if now.duration_since(request_time) > Duration::from_secs(10) {
                            let msg = Metadata::request(*piece);
                            let buf = msg.to_bencode()?;
                            self
                                .state
                                .sink
                                .send(Core::Extended(ExtendedMessage(ut_metadata, buf)))
                                .await?;

                            // update request time
                            request_time = Instant::now();
                        }
                    }
                }
                _ = request_interval.tick(), if self.can_request() && self.state.have_info => {
                    self.request_block_infos().await?;
                }
                _ = rerequest_timeout_interval.tick(), if self.state.have_info => {
                    self.check_request_timeout().await?;
                }
                _ = interested_interval.tick() => {
                    let should_be_interested = self.has_piece_not_in_local().await?;
                    debug!("should_be_interested {should_be_interested}");

                    if should_be_interested &&
                        !self.state.ctx.am_interested.load(Ordering::Relaxed)
                    {
                        info!("sending interested");
                        self.state.ctx.am_interested.store(true, Ordering::Relaxed);
                        self.state_log[1] = 'i';
                        self.state.sink.send(Core::Interested).await?;
                        self.state.sink.flush().await?;
                    }

                    // sorry, you're not the problem, it's me.
                    if !should_be_interested && self.state.ctx.am_interested.load(Ordering::Relaxed) {
                        debug!("sending not interested");
                        self.state.ctx.am_interested.store(false, Ordering::Relaxed);
                        self.state_log[1] = '-';
                        self.state.sink.send(Core::NotInterested).await?;
                        self.state.sink.flush().await?;
                    }
                }
                _ = keep_alive_interval.tick() => {
                    self.state.sink.send(Core::KeepAlive).await?;
                }
                Some(Ok(msg)) = self.state.stream.next() => {
                    match msg {
                        Core::Extended(msg @ ExtendedMessage(ext_id, _)) => {
                            info!("received ext {ext_id}");
                            match ext_id {
                                <Extended as ExtMsg>::ID => {
                                    let msg: Extended = msg.try_into()?;

                                    MsgHandler.handle_msg(
                                        self,
                                        msg,
                                    ).await?;
                                }
                                <Metadata as ExtMsg>::ID => {
                                    let msg: Result<Metadata, Error> = msg.try_into();

                                    if let Ok(meta) = msg {
                                        MsgHandler.handle_msg(
                                            self,
                                            meta,
                                        ).await?;
                                    } else {
                                        tracing::warn!("error decoding metadata msg");
                                    }

                                    // let msg: Metadata = msg.try_into()?;
                                }
                                _ => info!("other ext_id {ext_id}")
                            }
                        }
                        _ => {
                            MsgHandler.handle_msg(
                                self,
                                msg,
                            ).await?;
                        }
                    }
                }
                Some(msg) = self.state.rx.recv() => {
                    match msg {
                        PeerMsg::GetPieces(tx) => {
                            info!("peer get_pieces {:?}", self.state.pieces.len());
                            let _ = tx.send(self.state.pieces.clone());
                        }
                        PeerMsg::SendToSink(msg) => {
                            self.state.sink.send(msg).await?;
                        }
                        PeerMsg::HavePiece(piece) => {
                            debug!("has piece {piece}");

                            if let Some(b) = self.state.pieces.get(piece) {
                                // send Have to this peer if he doesnt have this piece
                                if !b {
                                    debug!("sending have {piece}");
                                    let _ = self.state.sink.send(Core::Have(piece)).await;
                                }
                            }
                        }
                        PeerMsg::RequestBlockInfos(block_infos) => {
                            debug!("request_block_infos len {}", block_infos.len());

                            let max = self.state.target_request_queue_len as usize - self.state.outgoing_requests.len();

                            if self.can_request() {
                                for block_info in block_infos.into_iter().take(max) {
                                    self.state.outgoing_requests.push(
                                        (block_info.clone(), Instant::now())
                                    );

                                    self.state.sink.send(Core::Request(block_info)).await?;
                                }
                            }
                        }
                        PeerMsg::NotInterested => {
                            debug!("sending not_interested");
                            self.state.ctx.am_interested.store(false, Ordering::Relaxed);
                            self.state_log[1] = '-';
                            self.state.sink.send(Core::NotInterested).await?;
                            self.state.sink.flush().await?;
                        }
                        PeerMsg::Interested => {
                            debug!("sending interested");
                            self.state.ctx.am_interested.store(true, Ordering::Relaxed);
                            self.state_log[1] = 'i';
                            self.state.sink.send(Core::Interested).await?;
                            self.state.sink.flush().await?;
                        }
                        PeerMsg::Choke => {
                            debug!("sending choke");
                            self.state.ctx.am_choking.store(true, Ordering::Relaxed);
                            self.state_log[0] = '-';
                            self.state.sink.send(Core::Choke).await?;
                            self.state.sink.flush().await?;
                        }
                        PeerMsg::Unchoke => {
                            debug!("sending unchoke");
                            self.state.ctx.am_choking.store(false, Ordering::Relaxed);
                            self.state_log[0] = 'u';
                            self.state.sink.send(Core::Unchoke).await?;
                            self.state.sink.flush().await?;
                        }
                        PeerMsg::Pause => {
                        }
                        PeerMsg::Resume => {
                        }
                        PeerMsg::CancelBlock(block_info) => {
                            debug!("cancel_block");
                            self.state.outgoing_requests.retain(|v| v.0 != block_info);
                            self.state.sink.send(Core::Cancel(block_info)).await?;
                            self.state.sink.flush().await?;
                        }
                        PeerMsg::SeedOnly => {
                            debug!("seed_only");
                            self.state.seed_only = true;
                        }
                        PeerMsg::GracefullyShutdown => {
                            debug!("gracefully_shutdown");
                            self.state.connection = ConnectionState::Quitting;
                            self.free_pending_blocks().await;
                            return Ok(());
                        }
                        PeerMsg::Quit => {
                            debug!("quit");
                            self.state.connection = ConnectionState::Quitting;
                            return Ok(());
                        }
                        PeerMsg::HaveInfo => {
                            self.state.have_info = true;
                            self.state.outgoing_requests_info_pieces.clear();
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
        let am_interested =
            self.state.ctx.am_interested.load(Ordering::Relaxed);
        let peer_choking = self.state.ctx.peer_choking.load(Ordering::Relaxed);
        let have_capacity = self.state.outgoing_requests.len()
            < self.state.target_request_queue_len as usize;

        am_interested
            && !peer_choking
            && self.state.have_info
            && have_capacity
            && !self.state.seed_only
    }

    /// Handle a new Piece msg from the peer, a Piece msg actually sends
    /// a block, and not a piece.
    pub async fn handle_piece_msg(
        &mut self,
        block: Block,
    ) -> Result<(), Error> {
        let block_info = BlockInfo::from(&block);

        // remove pending block request
        self.state.outgoing_requests.retain(|v| v.0 != block_info);

        // if in endgame, send cancels to all other peers
        if self.state.in_endgame {
            let from = self.state.ctx.id.clone();
            let _ = self
                .state
                .torrent_ctx
                .tx
                .send(TorrentMsg::SendCancelBlock {
                    from,
                    block_info: block_info.clone(),
                })
                .await;
        }

        // Update peer-specific counters
        self.state.ctx.counter.record_download(block_info.len as u64);

        self.state
            .ctx
            .downloaded
            .fetch_add(block_info.len as u64, Ordering::Relaxed);

        // Update torrent-level counters
        self.state
            .torrent_ctx
            .tx
            .send(TorrentMsg::IncrementDownloaded(block_info.len as u64))
            .await?;

        self.state
            .ctx
            .uploaded
            .fetch_add(block.block.len() as u64, Ordering::Relaxed);

        self.state
            .torrent_ctx
            .disk_tx
            .send(DiskMsg::WriteBlock {
                block,
                info_hash: self.state.torrent_ctx.info_hash.clone(),
            })
            .await?;

        Ok(())
    }

    /// Re-request blocks that timed-out
    async fn check_request_timeout(&mut self) -> Result<(), Error> {
        let now = Instant::now();

        // Identify timed-out requests
        for (block_info, mut request_time) in &self.state.outgoing_requests {
            if now.duration_since(request_time) >= BLOCK_TIMEOUT {
                debug!("rerequesting block {:?}", block_info);
                self.state.sink.send(Core::Request(block_info.clone())).await?;
                request_time = Instant::now();
            }
        }

        Ok(())
    }

    /// Take outgoing block infos that are in queue and send them back
    /// to the disk so that other peers can request those blocks.
    /// A good example to use this is when the Peer is no longer
    /// available (disconnected).
    pub async fn free_pending_blocks(&mut self) {
        let local = self.state.ctx.local_addr;
        let remote = self.state.ctx.remote_addr;

        let blocks: Vec<BlockInfo> =
            self.state.outgoing_requests.drain(..).map(|v| v.0).collect();

        debug!(
            "{local} freeing {:?} blocks for download of {remote}",
            blocks.len()
        );

        // send this block_info back to the vec of available block_infos,
        // so that other peers can download it.
        if !blocks.is_empty() {
            let _ = self
                .state
                .torrent_ctx
                .disk_tx
                .send(DiskMsg::ReturnBlockInfos(
                    self.state.torrent_ctx.info_hash.clone(),
                    blocks,
                ))
                .await;
        }
    }

    /// Request new block infos to this Peer's remote address.
    /// Must be used after checking that the Peer is able to send blocks with
    /// [`Self::can_request`].
    pub async fn request_block_infos(&mut self) -> Result<(), Error> {
        let target_request_queue_len =
            self.state.target_request_queue_len as usize;

        let current_requests = self.state.outgoing_requests.len();

        // the number of blocks we can request right now
        let request_len =
            target_request_queue_len.saturating_sub(current_requests);

        info!(
            "requesting block infos: {request_len} pending: {current_requests}"
        );

        if request_len == 0 {
            return Ok(());
        };

        // get a list of unique block_infos from the Disk,
        // those are already marked as requested on Torrent
        let (otx, orx) = oneshot::channel();
        self.state
            .torrent_ctx
            .disk_tx
            .send(DiskMsg::RequestBlocks {
                recipient: otx,
                qnt: request_len,
                info_hash: self.state.torrent_ctx.info_hash.clone(),
                peer_id: self.state.ctx.id.clone(),
                peer_pieces: self.state.pieces.clone(),
            })
            .await?;

        let block_infos = orx.await?;

        for block_info in block_infos {
            self.state
                .outgoing_requests
                .push((block_info.clone(), Instant::now()));

            let _ = self.state.sink.send(Core::Request(block_info)).await;
        }

        Ok(())
    }

    /// Start endgame mode. This will take the few remaining block infos
    /// and request them to all the peers of the torrent. After the first peer
    /// receives it, it send Cancel messages to all other peers.
    #[tracing::instrument(skip(self))]
    pub async fn start_endgame(&mut self) {
        self.state.in_endgame = true;

        let outgoing: Vec<BlockInfo> =
            self.state.outgoing_requests.drain(..).map(|v| v.0).collect();

        let _ = self
            .state
            .torrent_ctx
            .tx
            .send(TorrentMsg::StartEndgame(outgoing))
            .await;
    }

    /// Maybe request an info piece from this Peer if:
    /// - The peer supports the "ut_metadata" extension from the extension
    ///   protocol
    /// - We do not have the info downloaded
    pub async fn try_request_info(&mut self) -> Result<(), Error> {
        if self.state.have_info {
            return Ok(());
        }

        let Some(ut_metadata) = self
            .state
            .ext_states
            .extension
            .as_ref()
            .and_then(|v| v.m.ut_metadata)
        else {
            return Ok(());
        };

        let Some(meta_size) = self
            .state
            .ext_states
            .extension
            .as_ref()
            .and_then(|v| v.metadata_size)
        else {
            return Ok(());
        };

        // Calculate total pieces needed
        let total_pieces = meta_size.div_ceil(BLOCK_LEN as u64);
        debug!("total_pieces {total_pieces}");

        let requested_len = self.state.outgoing_requests_info_pieces.len();

        let max_to_request =
            (total_pieces as usize).saturating_sub(requested_len);

        let mut to_request_len = 0;

        // Determine which pieces we still need to request
        let mut needed_pieces = Vec::with_capacity(total_pieces as usize);

        for piece in 0..total_pieces {
            if to_request_len >= max_to_request {
                break;
            }
            if !self
                .state
                .outgoing_requests_info_pieces
                .iter()
                .any(|p| p.0 == piece)
            {
                to_request_len += 1;
                needed_pieces.push(piece);
            }
        }

        // Request up to available slots
        for piece in needed_pieces {
            let msg = Metadata::request(piece);
            let buf = msg.to_bencode()?;

            info!("requesting meta piece id: {piece}");

            // Track requested piece and request time
            self.state
                .outgoing_requests_info_pieces
                .push((piece, Instant::now()));

            self.state
                .sink
                .send(Core::Extended(ExtendedMessage(ut_metadata, buf)))
                .await?;
        }

        Ok(())
    }

    /// If this Peer has a piece that the local Peer (client)
    /// does not have.
    pub async fn has_piece_not_in_local(&self) -> Result<bool, Error> {
        let (otx, orx) = oneshot::channel();

        // local bitfield of the local peer
        self.state.torrent_ctx.tx.send(TorrentMsg::ReadBitfield(otx)).await?;

        let local_bitfield = orx.await?;

        // when we don't have the info fully downloaded yet,
        // and the peer has already sent a bitfield or a have.
        if local_bitfield.is_empty() {
            debug!("local bitfield is empty, returning true");
            return Ok(true);
        }

        // check that we don't loop out of bounds
        let min = local_bitfield.len().min(self.state.pieces.len());

        for i in 0..min {
            if !local_bitfield.get(i).unwrap()
                && *self.state.pieces.get(i).unwrap()
            {
                // we will become interested
                return Ok(true);
            }
        }

        Ok(false)
    }
}
