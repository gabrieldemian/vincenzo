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

use tracing::{debug, trace};

use crate::{
    disk::ReturnToDisk,
    extensions::{
        ExtMsg, ExtMsgHandler, Extended, ExtendedMessage, Metadata,
        MetadataPiece,
    },
    torrent::PeerBrMsg,
};

use crate::{
    disk::DiskMsg,
    error::Error,
    extensions::core::{Block, BlockInfo, Core},
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
        let mut metadata_interval = interval(Duration::from_millis(100));

        // request block infos
        let mut block_interval = interval(Duration::from_millis(100));

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

        let mut brx = self.state.ctx.torrent_ctx.btx.subscribe();

        loop {
            select! {
                _ = metadata_interval.tick(), if !self.state.have_info => {
                    self.request_metadata().await?;
                    self.rerequest_metadata().await?;
                }
                _ = block_interval.tick(), if self.can_request() => {
                    self.request_blocks().await?;
                    self.rerequest_blocks().await?;
                }
                _ = interested_interval.tick(), if !self.state.seed_only && !self.state.is_paused => {

                    // if !self.state.ctx.peer_choking.load(Ordering::Relaxed) {
                    //     tracing::info!(
                    //         "a {:?} b {:?} p {} tout {:?} avg {:?} l {:?}",
                    //         self.state.req_man_block.get_available_request_len(),
                    //         self.state.req_man_block.len(),
                    //         self.state.req_man_block.len_pieces(),
                    //         self.state.req_man_block.get_timeout(),
                    //         self.state.req_man_block.get_avg(),
                    //         self.state.req_man_block.last_response(),
                    //     );
                    // }
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
                        PeerBrMsg::Endgame(blocks) => {
                            self.start_endgame().await;
                            for block in blocks.into_values().flatten() {
                                self
                                    .state
                                    .req_man_block
                                    .add_request(block.clone());
                            }
                        }
                        PeerBrMsg::Request(blocks) => {
                            for block in blocks.into_values().flatten() {
                                self
                                    .state
                                    .req_man_block
                                    .add_request(block.clone());
                            }
                        }
                        PeerBrMsg::Cancel { block_info, ..} => {
                            if self.state.req_man_block.remove_request(&block_info) {
                                self.state.sink.send(Core::Cancel(block_info)).await?;
                            }
                        }
                        PeerBrMsg::HavePiece(piece) => {
                            self.state.sink.send(Core::Have(piece)).await?;
                        }
                        PeerBrMsg::Pause => {
                            debug!("pause");
                            self.state.is_paused = true;
                            self.state.ctx.tx.send(PeerMsg::Choke).await?;
                            self.state.ctx.tx.send(PeerMsg::NotInterested).await?;
                        }
                        PeerBrMsg::Resume => {
                            debug!("resume");
                            self.state.is_paused = false;
                        }
                        PeerBrMsg::Seedonly => {
                            debug!("seed_only");
                            self.state.seed_only = true;
                            self.state.ctx.tx.send(PeerMsg::Unchoke).await?;
                            self.state.req_man_meta.clear();
                            self.state.req_man_block.clear();
                        }
                        PeerBrMsg::Quit => {
                            debug!("quit");
                            return Ok(());
                        }
                        PeerBrMsg::HaveInfo => {
                            debug!("have_info");
                            self.state.have_info = true;
                            self.state.req_man_meta.clear();
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
            && !self.state.in_endgame
            && self.state.have_info
            && have_capacity
            && !self.state.seed_only
            && !self.state.is_paused
    }

    /// Handle a block sent by the core codec.
    pub async fn handle_block(&mut self, block: Block) -> Result<(), Error> {
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
            let _ = self.state.ctx.torrent_ctx.btx.send(PeerBrMsg::Cancel {
                from: self.state.ctx.id.clone(),
                block_info,
            });
        }

        Ok(())
    }

    /// Take outgoing block infos that are in queue and send them back
    /// to the disk so that other peers can request those blocks.
    /// A good example to use this is when the Peer is no longer
    /// available (disconnected).
    pub fn free_pending_blocks(&mut self) {
        if self.state.have_info {
            let blocks = self.state.req_man_block.drain();
            debug!("returning {} blocks", blocks.len());

            let _ = self.state.free_tx.send(ReturnToDisk::Block(
                self.state.ctx.torrent_ctx.info_hash.clone(),
                blocks,
            ));
        } else {
            let pieces = self.state.req_man_meta.drain();
            let pieces = pieces.into_values().flatten().collect::<Vec<_>>();
            debug!("returning {} pieces", pieces.len());

            let _ = self.state.free_tx.send(ReturnToDisk::Metadata(
                self.state.ctx.torrent_ctx.info_hash.clone(),
                pieces,
            ));
        }
    }

    /// Request new block infos to this Peer.
    /// Must be used after checking that the Peer is able to send blocks with
    /// [`Self::can_request`].
    pub async fn request_blocks(&mut self) -> Result<(), Error> {
        // max available requests for this peer at the current moment
        let qnt = self.state.req_man_block.get_available_request_len();

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
        let is_empty = blocks.is_empty();

        for block in blocks {
            if self.state.req_man_block.add_request(block.clone()) {
                self.state.sink.feed(Core::Request(block)).await?;
            }
        }

        if !is_empty {
            self.state.sink.flush().await?;
        }

        Ok(())
    }

    /// Check for timed out block requests and request them again.
    async fn rerequest_blocks(&mut self) -> Result<(), Error> {
        let blocks = self.state.req_man_block.get_timeout_blocks_and_update();
        debug!("rerequesting {}", blocks.len());

        let is_empty = blocks.is_empty();
        for block in blocks {
            self.state.sink.feed(Core::Request(block)).await?;
        }

        // todo: some peers dont resend the blocks no matter how many times we
        // resend, even if they have the piece. Maybe after 3 tries send all the
        // blocks to another peer.
        if !is_empty {
            self.state.sink.flush().await?;
        }

        Ok(())
    }

    /// Request available metadata pieces.
    pub async fn request_metadata(&mut self) -> Result<(), Error> {
        let Some(ut_metadata) = self
            .state
            .ext_states
            .extension
            .as_ref()
            .and_then(|v| v.m.ut_metadata)
        else {
            return Ok(());
        };

        let qnt = self.state.req_man_meta.get_available_request_len();

        if qnt == 0 {
            return Ok(());
        };

        let (otx, orx) = oneshot::channel();

        self.state
            .ctx
            .torrent_ctx
            .disk_tx
            .send(DiskMsg::RequestMetadata {
                recipient: otx,
                qnt,
                info_hash: self.state.ctx.torrent_ctx.info_hash.clone(),
            })
            .await?;

        let pieces: Vec<MetadataPiece> = orx.await?;

        for piece in &pieces {
            let msg = Metadata::request(piece.0 as u64);
            let buf = msg.to_bencode()?;
            self.state
                .sink
                .feed(Core::Extended(ExtendedMessage(ut_metadata, buf)))
                .await?;
        }

        if !pieces.is_empty() {
            self.state.sink.flush().await?;
        }

        Ok(())
    }

    /// Check for timedout metadata requests and request them again.
    pub async fn rerequest_metadata(&mut self) -> Result<(), Error> {
        let Some(ut_metadata) = self
            .state
            .ext_states
            .extension
            .as_ref()
            .and_then(|v| v.m.ut_metadata)
        else {
            return Ok(());
        };

        let pieces = self.state.req_man_meta.get_timeout_blocks_and_update();

        for piece in &pieces {
            let msg = Metadata::request(piece.0 as u64);
            let buf = msg.to_bencode()?;
            self.state
                .sink
                .feed(Core::Extended(ExtendedMessage(ut_metadata, buf)))
                .await?;
        }

        if !pieces.is_empty() {
            self.state.sink.flush().await?;
        }

        Ok(())
    }

    /// Start endgame mode. This will take the few pending block infos
    /// and request them to all downloading peers of the torrent. When the block
    /// arrives, send Cancel messages to all other peers.
    pub async fn start_endgame(&mut self) {
        self.state.in_endgame = true;
        let blocks = self.state.req_man_block.get_requests();
        let _ = self.state.ctx.torrent_ctx.btx.send(PeerBrMsg::Request(blocks));
    }
}
