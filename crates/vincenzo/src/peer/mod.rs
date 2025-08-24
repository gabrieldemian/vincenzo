//! A remote peer in the network that downloads and uploads data
mod types;

use bendy::encoding::ToBencode;
// re-exports
pub use types::*;
mod request_manager;
pub use request_manager::RequestManager;

use futures::{SinkExt, StreamExt};
use std::{sync::atomic::Ordering, time::Duration};
use tokio::{
    select,
    sync::oneshot,
    time::{interval, interval_at, Instant},
};

use tracing::{debug, info, trace};

use crate::{
    disk::ReturnBlockInfos,
    extensions::{
        ExtMsg, ExtMsgHandler, Extended, ExtendedMessage, Metadata,
        MetadataPiece,
    },
    torrent::TorrentBrMsg,
};

use crate::{
    disk::DiskMsg,
    error::Error,
    extensions::core::{Block, BlockInfo, Core, BLOCK_LEN},
    torrent::TorrentMsg,
};

/// Data about a remote Peer that the client is connected to,
/// but the client itself does not have a Peer struct.
#[derive(Default, PartialEq, Eq)]
pub struct Peer<S: PeerState> {
    pub state: S,
    pub(crate) state_log: StateLog,
}

/// Handle peer messages.
/// Each extension will use this type to implement a trait to handle messages of
/// its extension.
pub struct MsgHandler;

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
        self.state
            .ctx
            .torrent_ctx
            .tx
            .send(TorrentMsg::PeerConnected(self.state.ctx.clone()))
            .await?;

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

        // maybe send bitfield
        {
            let (otx, orx) = oneshot::channel();
            self.state
                .ctx
                .torrent_ctx
                .tx
                .send(TorrentMsg::ReadBitfield(otx))
                .await?;

            let bitfield = orx.await?;

            if bitfield.any() {
                debug!(
                    "> bitfield len: {} ones: {}",
                    bitfield.len(),
                    bitfield.count_ones()
                );

                self.state.sink.send(Core::Bitfield(bitfield)).await?;
            }
        }

        // when running a new Peer, we might
        // already have the info downloaded.
        {
            let info = self.state.ctx.torrent_ctx.info.read().await;
            self.state.have_info = info.piece_length > 0;
        }

        if !self.state.have_info {
            self.request_metadata().await?;
        }

        let mut brx = self.state.ctx.torrent_ctx.btx.subscribe();

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

                    // check for timed-out requests (3 seconds)
                    for piece in self.state.req_man_meta.get_timeout_blocks_and_update(
                        self.get_block_timeout(),
                        self.available_target_queue_len()
                    ) {
                        let msg = Metadata::request(piece.0 as u64);
                        let buf = msg.to_bencode()?;

                        debug!("re-requesting meta piece: {piece:?}");

                        self.state
                            .sink
                            .send(Core::Extended(ExtendedMessage(ut_metadata, buf)))
                            .await?;
                    }
                }
                _ = request_interval.tick(), if self.can_request() => {
                    self.request_blocks().await?;
                }
                _ = rerequest_timeout_interval.tick(), if self.can_request() => {
                    self.rerequest_timeout_blocks().await?;

                    info!("inflight b: {} inflight p {}",
                        self.state.req_man_block.len(),
                        self.state.req_man_block.len_pieces(),
                    );
                }
                _ = interested_interval.tick(), if !self.state.seed_only && !self.state.is_paused => {
                    let (otx, orx) = oneshot::channel();

                    self.state.ctx.torrent_ctx.tx.send(
                        TorrentMsg::PeerHasPieceNotInLocal(self.state.ctx.id.clone(), otx)
                    ).await?;

                    let should_be_interested = orx.await?;

                    debug!("should_be_interested {should_be_interested:?}");

                    if should_be_interested.is_some() &&
                        !self.state.ctx.am_interested.load(Ordering::Relaxed)
                    {
                        debug!("> interested");
                        self.state.ctx.am_interested.store(true, Ordering::Relaxed);
                        self.state_log[1] = 'i';
                        self.state.sink.send(Core::Interested).await?;
                    }

                    // sorry, you're not the problem, it's me.
                    if should_be_interested.is_none() && self.state.ctx.am_interested.load(Ordering::Relaxed) {
                        debug!("> not_interested");
                        self.state.ctx.am_interested.store(false, Ordering::Relaxed);
                        self.state_log[1] = '-';
                        self.state.sink.send(Core::NotInterested).await?;
                    }
                }
                _ = keep_alive_interval.tick() => {
                    self.state.sink.send(Core::KeepAlive).await?;
                }
                Ok(msg) = brx.recv() => {
                    match msg {
                        TorrentBrMsg::Endgame => {
                            self.start_endgame().await;
                        }
                        TorrentBrMsg::Request { blocks } => {
                            for block in blocks.into_values().flatten() {
                                if self
                                    .state
                                    .req_man_block
                                    .add_request(block.clone(), self.get_block_timeout())
                                {
                                    self.state.sink.feed(Core::Request(block)).await?;
                                }
                            }
                            self.state.sink.flush().await?;
                        }
                        TorrentBrMsg::Cancel { block_info, from} => {
                            if self.state.ctx.id == from { continue };
                            self.state.sink.send(Core::Cancel(block_info)).await?;
                        }
                    }
                }
                Some(Ok(msg)) = self.state.stream.next() => {
                    match msg {
                        Core::Extended(msg @ ExtendedMessage(ext_id, _)) => {
                            // todo: reduce this repetition somehow
                            match ext_id {
                                <Extended as ExtMsg>::ID => {
                                    let msg: Extended = msg.try_into()?;

                                    MsgHandler.handle_msg(
                                        self,
                                        msg,
                                    ).await?;
                                }
                                <Metadata as ExtMsg>::ID => {
                                    let msg: Metadata = msg.try_into()?;

                                    MsgHandler.handle_msg(
                                        self,
                                        msg,
                                    ).await?;
                                }
                                _ => {}
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
                        PeerMsg::HavePiece(piece) => {
                            self.state.sink.send(Core::Have(piece)).await?;
                        }
                        PeerMsg::RequestBlockInfos(block_infos) => {
                            for block_info in block_infos {
                                if self.state.req_man_block.add_request(
                                    block_info.clone(),
                                    self.get_block_timeout(),
                                ) {
                                    self.state.sink.feed(Core::Request(block_info)).await?;
                                }
                            }

                            self.state.sink.flush().await?;
                        }
                        PeerMsg::NotInterested => {
                            debug!("> not_interested");
                            self.state.ctx.am_interested.store(false, Ordering::Relaxed);
                            self.state_log[1] = '-';
                            self.state.sink.send(Core::NotInterested).await?;
                        }
                        PeerMsg::Interested => {
                            debug!("> interested");
                            self.state.ctx.am_interested.store(true, Ordering::Relaxed);
                            self.state_log[1] = 'i';
                            self.state.sink.send(Core::Interested).await?;
                        }
                        PeerMsg::Choke => {
                            debug!("> choke");
                            self.state.ctx.am_choking.store(true, Ordering::Relaxed);
                            self.state_log[0] = '-';
                            self.state.sink.send(Core::Choke).await?;
                        }
                        PeerMsg::Unchoke => {
                            debug!("> unchoke");
                            self.state.ctx.am_choking.store(false, Ordering::Relaxed);
                            self.state_log[0] = 'u';
                            self.state.sink.send(Core::Unchoke).await?;
                        }
                        PeerMsg::Pause => {
                            debug!("pause");
                            self.state.is_paused = true;
                            self.state.ctx.tx.send(PeerMsg::Choke).await?;
                            self.state.ctx.tx.send(PeerMsg::NotInterested).await?;
                        }
                        PeerMsg::Resume => {
                            debug!("resume");
                            self.state.is_paused = false;
                        }
                        PeerMsg::CancelBlock(block_info) => {
                            debug!("> cancel_block");
                            self.state.req_man_block.remove_request(&block_info);
                            self.state.sink.feed(Core::Cancel(block_info)).await?;
                            self.state.sink.flush().await?;
                        }
                        PeerMsg::SeedOnly => {
                            debug!("seed_only");
                            self.state.seed_only = true;
                            self.state.ctx.tx.send(PeerMsg::Unchoke).await?;
                            self.state.ctx.tx.send(PeerMsg::NotInterested).await?;
                            self.state.req_man_meta.clear();
                            self.state.req_man_block.clear();
                        }
                        PeerMsg::GracefullyShutdown => {
                            debug!("gracefully_shutdown");
                            return Ok(());
                        }
                        PeerMsg::Quit => {
                            debug!("quit");
                            return Ok(());
                        }
                        PeerMsg::HaveInfo => {
                            debug!("have_info");
                            self.state.have_info = true;
                            self.state.req_man_meta.clear();
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
        let have_capacity = self.state.req_man_block.len()
            < self.state.target_request_queue_len as usize;

        am_interested
            && !peer_choking
            && self.state.have_info
            && have_capacity
            && !self.state.seed_only
            && !self.state.is_paused
    }

    /// Handle a block sent by the core codec.
    pub async fn handle_block_msg(
        &mut self,
        block: Block,
    ) -> Result<(), Error> {
        let block_info = BlockInfo::from(&block);

        let was_requested =
            self.state.req_man_block.remove_request(&block_info);

        // ignore unsolicited blocks, could be a malicious peer, a bugged
        // client, etc.
        if !was_requested {
            debug!("< not requested block {block_info:?}");
            return Ok(());
        }

        self.state.ctx.counter.record_download(block_info.len as u64);

        self.state
            .ctx
            .torrent_ctx
            .disk_tx
            .send(DiskMsg::WriteBlock {
                block,
                info_hash: self.state.ctx.torrent_ctx.info_hash.clone(),
            })
            .await?;

        // if in endgame, send cancels to all other peers
        if self.state.in_endgame {
            let from = self.state.ctx.id.clone();
            let _ = self.state.ctx.torrent_ctx.btx.send(TorrentBrMsg::Cancel {
                from,
                block_info: block_info.clone(),
            });
        }

        Ok(())
    }

    /// Get when a block request should be expired (timed out)
    pub fn get_block_timeout(&self) -> Instant {
        // todo: calculate this dynamically based on the peer's speed.
        Instant::now() + Duration::from_secs(3)
    }

    // todo: some peers dont resend the blocks no matter how many times we
    // resend, even if they have the piece. Maybe after 3 tries send all the
    // blocks to another peer.
    async fn rerequest_timeout_blocks(&mut self) -> Result<(), Error> {
        let qnt = self.available_target_queue_len();

        let blocks = self
            .state
            .req_man_block
            .get_timeout_blocks_and_update(self.get_block_timeout(), qnt);

        for block in blocks {
            self.state.sink.feed(Core::Request(block)).await?;
        }

        self.state.sink.flush().await?;

        Ok(())
    }

    /// Take outgoing block infos that are in queue and send them back
    /// to the disk so that other peers can request those blocks.
    /// A good example to use this is when the Peer is no longer
    /// available (disconnected).
    pub fn free_pending_blocks(&mut self) {
        let blocks = self.state.req_man_block.drain();

        info!("returning {} blocks", blocks.len());

        // send this block_info back to the vec of available block_infos,
        // so that other peers can download it.
        let _ = self.state.free_tx.send(ReturnBlockInfos(
            self.state.ctx.torrent_ctx.info_hash.clone(),
            blocks,
        ));
    }

    /// How many more requests this peer can receive from local without
    /// dropping the connection.
    pub fn available_target_queue_len(&self) -> usize {
        let target_request_queue_len =
            self.state.target_request_queue_len as usize;

        let current_requests =
            self.state.req_man_block.len() + self.state.req_man_meta.len();

        // the number of blocks we can request right now
        target_request_queue_len.saturating_sub(current_requests)
    }

    /// Request new block infos to this Peer.
    /// Must be used after checking that the Peer is able to send blocks with
    /// [`Self::can_request`].
    pub async fn request_blocks(&mut self) -> Result<(), Error> {
        // max available requests for this peer at the current moment
        let qnt = self.available_target_queue_len();

        trace!("requesting block infos: {qnt}");

        if qnt == 0 {
            return Ok(());
        };

        // get a list of block_infos from the Disk,
        // these are blocks that the Peer has on it's bitfield, and that the
        // local peer doesn't
        let (otx, orx) = oneshot::channel();
        self.state
            .ctx
            .torrent_ctx
            .disk_tx
            .send(DiskMsg::RequestBlocks {
                recipient: otx,
                qnt,
                peer_id: self.state.ctx.id.clone(),
            })
            .await?;

        let blocks: Vec<BlockInfo> = orx.await?;

        for block in blocks {
            if self
                .state
                .req_man_block
                .add_request(block.clone(), self.get_block_timeout())
            {
                self.state.sink.feed(Core::Request(block)).await?;
            }
        }

        self.state.sink.flush().await?;

        Ok(())
    }

    // todo: use start_endgame, disk will detect if all pieces has been
    // requested and send a message
    // also get the timeout blocks and send to torrent.
    //
    // all peers will get their pending blocks and send to the torrent.

    /// Start endgame mode. This will take the few pending block infos
    /// and request them to all downloading peers of the torrent. When the block
    /// arrives, send Cancel messages to all other peers.
    #[tracing::instrument(skip_all)]
    pub async fn start_endgame(&mut self) {
        self.state.in_endgame = true;

        let blocks = self.state.req_man_block.get_requests();

        let _ = self
            .state
            .ctx
            .torrent_ctx
            .btx
            .send(TorrentBrMsg::Request { blocks });
    }

    /// Try to request the metadata (info) if:
    /// - The peer supports the "ut_metadata" extension from the extension
    ///   protocol
    /// - We do not have the info downloaded
    pub async fn request_metadata(&mut self) -> Result<(), Error> {
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

        let total_pieces = meta_size.div_ceil(BLOCK_LEN as u64);
        debug!("total_pieces {total_pieces}");

        for piece in 0..total_pieces {
            if self.state.req_man_meta.add_request(
                MetadataPiece(piece as usize),
                self.get_block_timeout(),
            ) {
                let msg = Metadata::request(piece);
                let buf = msg.to_bencode()?;

                debug!("requesting meta piece id: {piece}");

                self.state
                    .sink
                    .feed(Core::Extended(ExtendedMessage(ut_metadata, buf)))
                    .await?;
            }
        }

        self.state.sink.flush().await?;

        Ok(())
    }
}
