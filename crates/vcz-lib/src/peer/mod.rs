//! A remote peer in the network that downloads and uploads data
mod request_manager;
mod types;

// re-exports
pub use request_manager::RequestManager;
pub use types::*;

use crate::{
    disk::{DiskMsg, ReturnToDisk},
    error::Error,
    extensions::{
        ExtMsgHandler, ExtendedMessage, Extension, Metadata, MetadataPiece,
        core::{Block, BlockInfo, Core},
    },
    torrent::{PeerBrMsg, TorrentMsg},
};
use bendy::encoding::ToBencode;
use futures::{SinkExt, StreamExt};
use std::{sync::atomic::Ordering, time::Duration};
use tokio::{
    select,
    sync::oneshot,
    time::{Instant, interval, interval_at},
};
use tracing::{debug, trace};

/// Data about a remote Peer that the client is connected to,
/// but the client itself does not have a Peer struct.
#[derive(Default, PartialEq, Eq, vcz_macros::Peer)]
#[extensions(Core, Metadata, Extension)]
pub struct Peer<S: PeerState> {
    pub state: S,
    /// am_choking[0], am_interested[1], peer_choking[2], peer_interested[3]
    pub state_log: StateLog,
}

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

        // request metadata pieces
        let mut metadata_interval = interval(Duration::from_millis(100));

        // request blocks
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

        let mut brx = self.state.ctx.torrent_ctx.btx.subscribe();

        loop {
            select! {
                Some(Ok(msg)) = self.state.stream.next() => {
                    // created by Peer macro in [`vcz_macros`]
                    let _ = self.handle_message(msg).await;
                }
                _ = metadata_interval.tick(), if !self.state.have_info => {
                    self.request_metadata().await?;
                    self.rerequest_metadata().await?;
                }
                _ = block_interval.tick(), if self.can_request() => {
                    // some intervals are only ran in production (not debug)
                    // because I want to run this deterministically
                    // and manually in integration tests.
                    #[cfg(not(feature = "debug"))]
                    self.request_blocks_disk().await?;

                    #[cfg(not(feature = "debug"))]
                    self.rerequest_blocks().await?;
                }
                _ = interested_interval.tick(),
                    if !self.state.seed_only && !self.state.is_paused
                => {
                    #[cfg(not(feature = "debug"))]
                    self.interested().await?;
                }
                _ = keep_alive_interval.tick() => {
                    self.send(Core::KeepAlive).await?;
                }
                Ok(msg) = brx.recv() => {
                    match msg {
                        PeerBrMsg::NewPeer(ctx) => {
                            if self.state.in_endgame && self.can_request() {
                                let blocks = self.state.req_man_block.get_requests();
                                let _ = ctx.tx.send(PeerMsg::Blocks(blocks)).await;
                            }
                        }
                        PeerBrMsg::Endgame(blocks) => {
                            self.start_endgame().await;
                            self.state
                                .req_man_block
                                .extend(blocks);
                        }
                        PeerBrMsg::Request(blocks) => {
                            self.state
                                .req_man_block
                                .extend(blocks);
                        }
                        PeerBrMsg::Cancel(block_info) => {
                            if self.state.req_man_block.remove_request(&block_info) {
                                self.send(Core::Cancel(block_info)).await?;
                            }
                        }
                        PeerBrMsg::HavePiece(piece) => {
                            self.send(Core::Have(piece)).await?;
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
                            self.seed_only().await?;
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
                Some(msg) = self.state.rx.recv() => {
                    match msg {
                        PeerMsg::InterestedAlgorithm => {
                            self.interested().await?;
                        }
                        PeerMsg::RequestAlgorithm => {
                            self.request_block_infos().await?;
                        }
                        PeerMsg::CloneBlocks(qnt, tx) => {
                            let reqs = self.state.req_man_block.clone_requests(qnt);
                            let _ = tx.send(reqs);
                        }
                        PeerMsg::Blocks(blocks) => {
                            self.add_block_infos(blocks).await?;
                        }
                        PeerMsg::NotInterested => {
                            debug!("> not_interested");
                            self.state.ctx.am_interested.store(false, Ordering::Release);
                            self.state_log[1] = '-';
                            self.send(Core::NotInterested).await?;
                        }
                        PeerMsg::Interested => {
                            debug!("> interested");
                            self.state.ctx.am_interested.store(true, Ordering::Release);
                            self.state_log[1] = 'i';
                            self.send(Core::Interested).await?;
                        }
                        PeerMsg::Choke => {
                            debug!("> choke");
                            self.state.ctx.am_choking.store(true, Ordering::Release);
                            self.state_log[0] = '-';
                            self.send(Core::Choke).await?;
                        }
                        PeerMsg::Unchoke => {
                            debug!("> unchoke");
                            self.state.ctx.am_choking.store(false, Ordering::Release);
                            self.state_log[0] = 'u';
                            self.send(Core::Unchoke).await?;
                        }
                    }
                }
            }
        }
    }

    /// Run interested algorithm.
    #[inline]
    pub async fn interested(&mut self) -> Result<(), Error> {
        if !self.state.ctx.peer_choking.load(Ordering::Acquire)
            && self.state.ctx.am_interested.load(Ordering::Acquire)
        {
            tracing::debug!(
                "b {} d {} t {:?} avg {:?}",
                self.state.req_man_block.len(),
                self.state.req_man_block.recv_count,
                self.state.req_man_block.get_timeout(),
                self.state.req_man_block.get_avg(),
            );
        }
        let (otx, orx) = oneshot::channel();

        self.state
            .ctx
            .torrent_ctx
            .tx
            .send(TorrentMsg::PeerHasPieceNotInLocal(
                self.state.ctx.id.clone(),
                otx,
            ))
            .await?;

        let should_be_interested = orx.await?;

        let am_interested =
            self.state.ctx.am_interested.load(Ordering::Acquire);

        if should_be_interested.is_some() && !am_interested {
            self.state.ctx.am_interested.store(true, Ordering::Release);
            self.state_log[1] = 'i';
            self.send(Core::Interested).await?;
        }

        // sorry, you're not the problem, it's me.
        if should_be_interested.is_none() && am_interested {
            self.state.ctx.am_interested.store(false, Ordering::Release);
            self.state_log[1] = '-';
            self.state.sink.send(Core::NotInterested).await?;
        }
        Ok(())
    }

    #[inline]
    /// Enter seed only mode and send Cancel's for in-flight block infos.
    pub async fn seed_only(&mut self) -> Result<(), Error> {
        debug!("seed_only");
        self.state.seed_only = true;
        self.state.ctx.tx.send(PeerMsg::Unchoke).await?;

        for block in self.state.req_man_block.drain().into_iter() {
            self.send(Core::Cancel(block)).await?;
        }

        self.state.req_man_meta.clear();

        Ok(())
    }

    /// Check if we can request new blocks, if:
    /// - We are not being choked by the peer
    /// - We are interested in the peer
    /// - We have the downloaded the info of the torrent
    /// - The torrent is not fully downloaded (peer is not in seed-only mode)
    /// - The capacity of inflight blocks is not full (len of outgoing_requests)
    #[inline]
    pub fn can_request(&self) -> bool {
        let am_interested =
            self.state.ctx.am_interested.load(Ordering::Acquire);
        let peer_choking = self.state.ctx.peer_choking.load(Ordering::Acquire);
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
    #[inline]
    pub async fn handle_block(&mut self, block: Block) -> Result<(), Error> {
        let block_info = BlockInfo::from(&block);

        let was_requested =
            self.state.req_man_block.remove_request(&block_info);

        // ignore unsolicited (or duplicate) blocks, could be a malicious peer,
        // a bugged client, etc. Or when the client has sent a cancel
        // but because of the latency, the peer doesn't know that yet.
        if !was_requested {
            return Ok(());
        }

        // `downloaded` in the perspective of the local peer.
        self.state.ctx.counter.record_download(block_info.len as u64);

        // if in endgame, send cancels to all other peers
        if self.state.in_endgame {
            let _ = self
                .state
                .ctx
                .torrent_ctx
                .btx
                .send(PeerBrMsg::Cancel(block_info));
        }

        self.state
            .ctx
            .torrent_ctx
            .disk_tx
            .send(DiskMsg::WriteBlock {
                block,
                info_hash: self.state.ctx.torrent_ctx.info_hash.clone(),
            })
            .await?;

        Ok(())
    }

    /// Take outgoing block infos and metadata pieces and send them back to the
    /// disk so that other peers can request them.
    pub fn free_pending_blocks(&mut self) {
        let blocks = self.state.req_man_block.drain();

        tracing::debug!("returning {} blocks", blocks.len(),);

        if !blocks.is_empty() {
            let _ = self.state.free_tx.send(ReturnToDisk::Block(
                self.state.ctx.torrent_ctx.info_hash.clone(),
                blocks,
            ));
        }

        let pieces = self.state.req_man_meta.drain();
        debug!("returning {} pieces", pieces.len());

        if !pieces.is_empty() {
            let _ = self.state.free_tx.send(ReturnToDisk::Metadata(
                self.state.ctx.torrent_ctx.info_hash.clone(),
                pieces,
            ));
        }
    }

    /// Add block infos to this peer.
    #[inline]
    pub async fn add_block_infos(
        &mut self,
        blocks: Vec<BlockInfo>,
    ) -> Result<(), Error> {
        for block in blocks
            .into_iter()
            .take(self.state.req_man_block.get_available_request_len())
        {
            if self.state.req_man_block.add_request(block.clone()) {
                self.feed(Core::Request(block.clone())).await?;
            }
        }
        self.state.sink.flush().await?;
        Ok(())
    }

    /// Request block infos from Disk.
    /// Must be used after checking that the Peer is able to send blocks with
    /// [`Self::can_request`].
    #[inline]
    pub async fn request_block_infos(&mut self) -> Result<(), Error> {
        // max available requests for this peer at the current moment
        let qnt = self.state.req_man_block.get_available_request_len();

        if qnt == 0 {
            return Ok(());
        };

        // if the torrent is beign downloaded and the peer is out of block infos
        // to request, the peer will request more. This usually happens
        // for fast peers at the end of the download.
        let no_block_infos_and_torrent_downloading =
            self.state.req_man_block.is_requests_empty()
                && self.state.req_man_block.recv_count > 0
                && self.state.req_man_block.req_count > 0;

        if no_block_infos_and_torrent_downloading {
            self.state
                .ctx
                .torrent_ctx
                .tx
                .send(TorrentMsg::CloneBlockInfosToPeer(
                    qnt,
                    self.state.ctx.tx.clone(),
                ))
                .await?;
        }

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
                self.feed(Core::Request(block)).await?;
            }
        }

        if !is_empty {
            self.state.sink.flush().await?;
        }

        Ok(())
    }

    /// Check for timed out block requests and request them again.
    #[allow(dead_code)]
    async fn rerequest_blocks(&mut self) -> Result<(), Error> {
        let blocks = self.state.req_man_block.get_timeout_blocks_and_update();
        trace!("rerequesting {} blocks", blocks.len());

        let is_empty = blocks.is_empty();
        for block in blocks {
            self.feed(Core::Request(block)).await?;
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
        let Some(ut_metadata) =
            self.state.extension.as_ref().and_then(|v| v.m.ut_metadata)
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
            if self.state.req_man_meta.add_request(MetadataPiece(piece.0)) {
                let msg = Metadata::request(piece.0 as u64);
                let buf = msg.to_bencode()?;
                self.feed(Core::Extended(ExtendedMessage(ut_metadata, buf)))
                    .await?;
            }
        }

        if !pieces.is_empty() {
            self.state.sink.flush().await?;
        }

        Ok(())
    }

    /// Check for timedout metadata requests and request them again.
    pub async fn rerequest_metadata(&mut self) -> Result<(), Error> {
        let Some(ut_metadata) =
            self.state.extension.as_ref().and_then(|v| v.m.ut_metadata)
        else {
            return Ok(());
        };

        let pieces = self.state.req_man_meta.get_timeout_blocks_and_update();

        for piece in &pieces {
            let msg = Metadata::request(piece.0 as u64);
            let buf = msg.to_bencode()?;
            self.feed(Core::Extended(ExtendedMessage(ut_metadata, buf)))
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
    #[inline]
    pub async fn start_endgame(&mut self) {
        self.state.in_endgame = true;
        let blocks = self.state.req_man_block.get_requests();
        let _ = self.state.ctx.torrent_ctx.btx.send(PeerBrMsg::Request(blocks));
    }

    /// Send a message to sink and record upload rate, but the sink is not
    /// flushed.
    #[inline]
    pub async fn feed(&mut self, core: Core) -> Result<(), Error> {
        self.state.ctx.counter.record_upload(4 + core.len() as u64);
        self.state.sink.feed(core).await?;
        Ok(())
    }

    /// Send a message to sink and record upload rate and flush.
    #[inline]
    pub async fn send(&mut self, core: Core) -> Result<(), Error> {
        self.state.ctx.counter.record_upload(4 + core.len() as u64);
        self.state.sink.send(core).await?;
        Ok(())
    }

    /// Mutate the peer based on his [`Extension`], should be called after an
    /// extended handshake.
    #[inline]
    pub async fn handle_ext(&mut self, ext: Extension) -> Result<(), Error> {
        let n = ext.reqq.unwrap_or(DEFAULT_REQUEST_QUEUE_LEN);

        self.state.target_request_queue_len = n;
        self.state.req_man_meta.set_limit(n as usize);
        self.state.req_man_block.set_limit(n as usize);

        if let Some(meta_size) = ext.metadata_size {
            self.state
                .ctx
                .torrent_ctx
                .tx
                .send(TorrentMsg::MetadataSize(meta_size))
                .await?;
        }

        self.state.extension = Some(ext);

        Ok(())
    }
}
