//! A remote peer in the network that downloads and uploads data
pub mod session;
mod types;

// re-exports
pub use types::*;

use futures::{SinkExt, StreamExt};
use std::{collections::VecDeque, time::Duration};
use tokio::{
    select,
    sync::oneshot,
    time::{interval, interval_at, Instant},
};

use tracing::{debug, warn};

use crate::extensions::{
    ExtMsg, ExtMsgHandler, Extended, Metadata, MetadataMsg,
};

use crate::{
    disk::DiskMsg,
    error::Error,
    extensions::core::{Block, BlockInfo, Core, CoreId, BLOCK_LEN},
    peer::session::ConnectionState,
    torrent::TorrentMsg,
};

use self::session::Session;

/// Data about a remote Peer that the client is connected to,
/// but the client itself does not have a Peer struct.
pub struct Peer<S: PeerState> {
    pub state: S,
}

/// Handle peer messages.
/// Each extension will use this type to implement a trait to handle messages of
/// its extension.
pub struct MsgHandler;

impl Peer<Connected> {
    /// Start the event loop of the Peer, listen to messages sent by others
    /// on the peer wire protocol.
    #[tracing::instrument(skip_all, name = "peer::run")]
    pub async fn run(&mut self) -> Result<(), Error> {
        self.state.session.connection = ConnectionState::Connecting;

        let _ = self
            .state
            .torrent_ctx
            .tx
            .send(TorrentMsg::PeerConnected(self.state.ctx.clone()))
            .await;

        let local = self.state.ctx.local_addr;
        let remote = self.state.ctx.remote_addr;

        let mut tick_timer = interval(Duration::from_secs(1));

        let mut keep_alive_timer = interval_at(
            Instant::now() + Duration::from_secs(120),
            Duration::from_secs(120),
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
            debug!("{remote} sending bitfield");
            self.state.sink.send(Core::Bitfield(bitfield)).await?;
        }

        // when running a new Peer, we might
        // already have the info downloaded.
        {
            let info = self.state.torrent_ctx.info.read().await;
            self.state.have_info = info.piece_length > 0;
        }

        self.state.session.connection = ConnectionState::Connected;

        loop {
            select! {
                // update internal data every 1 second
                _ = tick_timer.tick(), if self.state.have_info => {
                    self.tick().await?;
                }
                // send Keepalive every 2 minutes
                _ = keep_alive_timer.tick(), if self.state.have_info => {
                    self.state.sink.send(Core::KeepAlive).await?;
                }
                Some(Ok(msg)) = self.state.stream.next() => {
                    match msg {
                        Core::Extended(ext) => {
                            match ext.0 {
                                <Extended as ExtMsg>::ID => {
                                    let msg: Extended = ext.try_into()?;
                                    MsgHandler.handle_msg(
                                        self,
                                        msg,
                                    ).await?;
                                }
                                <MetadataMsg as ExtMsg>::ID => {
                                    let msg: MetadataMsg = ext.try_into()?;
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
                        PeerMsg::GetPieces(tx) => {
                            let _ = tx.send(self.state.pieces.clone());
                        }
                        PeerMsg::SendToSink(msg) => {
                            self.state.sink.send(msg).await?;
                        }
                        PeerMsg::HavePiece(piece) => {
                            debug!("{remote} has piece {piece}");

                            if let Some(b) = self.state.pieces.get(piece) {
                                // send Have to this peer if he doesnt have this piece
                                if !b {
                                    debug!("{remote} sending have {piece}");
                                    let _ = self.state.sink.send(Core::Have(piece)).await;
                                }
                            }
                        }
                        PeerMsg::RequestBlockInfos(block_infos) => {
                            debug!("{remote} RequestBlockInfos len {}", block_infos.len());

                            let max = self.state.session.target_request_queue_len as usize - self.state.outgoing_requests.len();

                            if self.can_request() {
                                self.state.session.last_outgoing_request_time = Some(Instant::now());

                                for block_info in block_infos.into_iter().take(max) {
                                    self.state.outgoing_requests.insert(
                                        block_info.clone()
                                    );

                                    self.state.outgoing_requests_timeout
                                        .insert(block_info.clone(), Instant::now());

                                    self.state.sink.send(Core::Request(block_info)).await?;
                                }
                            }
                        }
                        PeerMsg::NotInterested => {
                            debug!("{remote} sending NotInterested");
                            self.state.ext_states.core.am_interested = false;
                            self.state.sink.send(Core::NotInterested).await?;
                        }
                        PeerMsg::Interested => {
                            debug!("{remote} sending Interested");
                            self.state.ext_states.core.am_interested = true;
                            self.state.sink.send(Core::Interested).await?;
                        }
                        PeerMsg::Choke => {
                            debug!("{remote} sending Choke");
                            self.state.ext_states.core.am_choking = true;
                            self.state.sink.send(Core::Choke).await?;
                        }
                        PeerMsg::Unchoke => {
                            debug!("{remote} sending Unchoke");
                            self.state.ext_states.core.am_choking = false;
                            self.state.sink.send(Core::Unchoke).await?;
                        }
                        PeerMsg::Pause => {
                            debug!("{remote} Pause");
                            self.state.session.prev_peer_choking = self.state.ext_states.core.peer_choking;

                            if self.state.ext_states.core.am_interested {
                                self.state.ext_states.core.am_interested = false;
                                let _ = self.state.sink.send(Core::NotInterested).await;
                            }

                            if !self.state.ext_states.core.peer_choking {
                                self.state.ext_states.core.peer_choking = true;
                                let _ = self.state.sink.send(Core::Choke).await;
                            }

                            for block_info in &self.state.outgoing_requests {
                                self.state.sink.send(Core::Cancel(block_info.clone())).await?;
                            }

                            self.free_pending_blocks().await;
                        }
                        PeerMsg::Resume => {
                            debug!("{local} Resume");
                            self.state.ext_states.core.peer_choking = self.state.session.prev_peer_choking;

                            if !self.state.ext_states.core.peer_choking {
                                self.state.sink.send(Core::Unchoke).await?;
                            }

                            let peer_has_piece = self.has_piece_not_in_local().await?;

                            if peer_has_piece {
                                debug!("{remote} we are interested due to Bitfield");
                                self.state.ext_states.core.am_interested = true;
                                self.state.sink.send(Core::Interested).await?;

                                if self.can_request() {
                                    self.request_block_infos().await?;
                                }
                            }
                        }
                        PeerMsg::CancelBlock(block_info) => {
                            debug!("{remote} CancelBlock");
                            self.state.outgoing_requests.remove(&block_info);
                            self.state.outgoing_requests_timeout.remove(&block_info);
                            self.state.sink.send(Core::Cancel(block_info)).await?;
                        }
                        PeerMsg::SeedOnly => {
                            debug!("{remote} SeedOnly");
                            self.state.session.seed_only = true;
                        }
                        PeerMsg::GracefullyShutdown => {
                            debug!("{remote} GracefullyShutdown");
                            self.state.session.connection = ConnectionState::Quitting;
                            self.free_pending_blocks().await;
                            return Ok(());
                        }
                        PeerMsg::Quit => {
                            debug!("{remote} Quit");
                            self.state.session.connection = ConnectionState::Quitting;
                            return Ok(());
                        }
                        PeerMsg::HaveInfo => {
                            debug!("{remote} HaveInfo");
                            self.state.have_info = true;
                            let am_interested = self.state.ext_states.core.am_interested;
                            let peer_choking = self.state.ext_states.core.peer_choking;

                            debug!("{remote} am_interested {am_interested}");
                            debug!("{remote} peer_choking {peer_choking}");

                            if am_interested && !peer_choking {
                                self.prepare_for_download().await;
                                debug!("{remote} requesting blocks");
                                self.request_block_infos().await?;
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
        let am_interested = self.state.ext_states.core.am_interested;
        let am_choking = self.state.ext_states.core.am_choking;
        let have_capacity = self.state.outgoing_requests.len()
            < self.state.session.target_request_queue_len as usize;

        am_interested
            && !am_choking
            && self.state.have_info
            && have_capacity
            && !self.state.session.seed_only
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
        self.state.outgoing_requests.remove(&block_info);
        self.state.outgoing_requests_timeout.remove(&block_info);

        // if in endgame, send cancels to all other peers
        if self.state.session.in_endgame {
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

        // let (tx, rx) = oneshot::channel();

        self.state
            .torrent_ctx
            .disk_tx
            .send(DiskMsg::WriteBlock {
                block,
                // recipient: tx,
                info_hash: self.state.torrent_ctx.info_hash.clone(),
            })
            .await?;

        // rx.await??;

        // update stats
        self.state.session.update_download_stats(len as u32);

        Ok(())
    }

    /// Periodic tick of the [`Peer`]. This function must be called every 1
    /// seconds to:
    /// - Check if we can request blocks.
    /// - Check and resend requests that timed out.
    /// - Update stats about the Peer.
    pub async fn tick(&mut self) -> Result<(), Error> {
        // resend requests if we have any pending and more time has elapsed
        // since the last received block than the current timeout value
        if !self.state.outgoing_requests.is_empty() && self.can_request() {
            self.check_request_timeout().await?;
        }

        self.state.session.counters.reset();

        Ok(())
    }

    /// Re-request blocks that timed-out
    async fn check_request_timeout(&mut self) -> Result<(), Error> {
        let local = self.state.ctx.local_addr;

        // if self.session.timed_out_request_count >= 10 {
        //     self.free_pending_blocks().await;
        //     return Ok(());
        // }

        for (block, timeout) in self.state.outgoing_requests_timeout.iter_mut()
        {
            let elapsed_since_last_request =
                Instant::now().saturating_duration_since(*timeout);

            // if the timeout time has already passed,
            if elapsed_since_last_request
                >= self.state.session.request_timeout()
            {
                self.state.session.register_request_timeout();

                debug!(
                    "{local} this block {block:#?} timed out ({} ms ago)",
                    elapsed_since_last_request.as_millis(),
                );

                let _ =
                    self.state.sink.send(Core::Request(block.clone())).await;
                *timeout = Instant::now();

                debug!(
                    "{local} timeout, total: {}",
                    self.state.session.timed_out_request_count + 1
                );
            }
        }
        if !self.state.session.seed_only {
            self.request_block_infos().await?;
        }

        Ok(())
    }

    /// Take the block infos that are in queue and send them back
    /// to the disk so that other peers can request those blocks.
    /// A good example to use this is when the Peer is no longer
    /// available (disconnected).
    pub async fn free_pending_blocks(&mut self) {
        let local = self.state.ctx.local_addr;
        let remote = self.state.ctx.remote_addr;
        let blocks: VecDeque<BlockInfo> =
            self.state.outgoing_requests.drain().collect();
        self.state.outgoing_requests_timeout.clear();

        self.state.session.timed_out_request_count = 0;

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
    #[tracing::instrument(level="debug", skip_all, fields(self.local_addr))]
    pub async fn request_block_infos(&mut self) -> Result<(), Error> {
        if self.state.session.seed_only {
            warn!("Calling request_block_infos when peer is in seed-only mode");
            return Ok(());
        }
        let local = self.state.ctx.local_addr;
        // let remote = self.state.ctx.remote_addr;

        let target_request_queue_len =
            self.state.session.target_request_queue_len as usize;

        // the number of blocks we can request right now
        let request_len =
            if self.state.outgoing_requests.len() >= target_request_queue_len {
                0
            } else {
                target_request_queue_len - self.state.outgoing_requests.len()
            };

        debug!("inflight: {}", self.state.outgoing_requests.len());
        debug!("max to request: {}", target_request_queue_len);
        debug!("request_len: {request_len}");
        debug!("target_request_queue_len: {target_request_queue_len}");

        if request_len > 0 {
            debug!("{local} peer requesting l: {:?} block infos", request_len);
            // get a list of unique block_infos from the Disk,
            // those are already marked as requested on Torrent
            let (otx, orx) = oneshot::channel();
            let _ = self
                .state
                .torrent_ctx
                .disk_tx
                .send(DiskMsg::RequestBlocks {
                    recipient: otx,
                    qnt: request_len,
                    info_hash: self.state.torrent_ctx.info_hash.clone(),
                    peer_id: self.state.ctx.id.clone(),
                })
                .await;

            let r = orx.await?;

            let f = r.front();
            debug!("first block requested {f:?}");

            if r.is_empty()
                && !self.state.session.in_endgame
                && self.state.outgoing_requests.len() <= 20
            {
                // self.start_endgame().await;
            }

            debug!("disk sent {:?} blocks", r.len());
            self.state.session.last_outgoing_request_time =
                Some(Instant::now());

            for block_info in r {
                // debug!("{local} requesting \n {block_info:#?} to {remote}");
                self.state.outgoing_requests.insert(block_info.clone());

                let _ = self
                    .state
                    .sink
                    .send(Core::Request(block_info.clone()))
                    .await;

                self.state
                    .outgoing_requests_timeout
                    .insert(block_info, Instant::now());

                let req_id: u64 = CoreId::Request as u64;

                self.state.session.counters.protocol.up += req_id;
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
        self.state.session.in_endgame = true;

        let outgoing: Vec<BlockInfo> =
            self.state.outgoing_requests.drain().collect();
        let _ = self
            .state
            .torrent_ctx
            .tx
            .send(TorrentMsg::StartEndgame(self.state.ctx.id.clone(), outgoing))
            .await;
    }

    /// Maybe request an info piece from this Peer if:
    /// - The peer supports the "ut_metadata" extension from the extension
    ///   protocol
    /// - We do not have the info downloaded
    #[tracing::instrument(skip(self))]
    pub async fn try_request_info(&mut self) -> Result<(), Error> {
        // only request info if we dont have an Info
        // and the peer supports the metadata extension protocol
        if self.state.have_info {
            return Ok(());
        }
        // send bep09 request to get the Info
        let Some(lt_metadata) = self
            .state
            .ext_states
            .extension
            .as_ref()
            .and_then(|v| v.m.lt_metadata)
        else {
            return Ok(());
        };

        debug!("peer supports lt_metadata {lt_metadata}, sending request");

        let Some(t) = self
            .state
            .ext_states
            .extension
            .as_ref()
            .and_then(|v| v.metadata_size)
        else {
            return Ok(());
        };

        let pieces = t as f32 / BLOCK_LEN as f32;
        let pieces = pieces.ceil() as u32;

        debug!("this info has {pieces} pieces");

        for i in 0..pieces {
            let h = Metadata::request(i);

            debug!("requesting info piece {i}");
            debug!("request {h:?}");

            // let h = h.to_bencode().map_err(|_| Error::BencodeError)?;
            // todo: fix this
            // let _ = self
            //     .sink
            //     .send(Core::Extended(ut_metadata, h).into())
            //     .await;
        }
        Ok(())
    }

    /// If this Peer has a piece that the local Peer (client)
    /// does not have.
    pub async fn has_piece_not_in_local(&self) -> Result<bool, Error> {
        let (otx, orx) = oneshot::channel();

        // local bitfield of the local peer
        let _ =
            self.state.torrent_ctx.tx.send(TorrentMsg::ReadBitfield(otx)).await;
        let local_bitfield = orx.await?;

        // when we don't have the info fully downloaded yet,
        // and the peer has already sent a bitfield or a have.
        if local_bitfield.is_empty() {
            return Ok(true);
        }

        for (local_piece, piece) in
            local_bitfield.iter().zip(self.state.pieces.iter())
        {
            if *piece && !local_piece {
                return Ok(true);
            }
        }

        Ok(false)
    }

    /// Calculate the maximum number of block infos to request,
    /// and set this value on `session` of the peer.
    pub async fn prepare_for_download(&mut self) {
        // the max number of block_infos to request
        let n = self
            .state
            .ext_states
            .extension
            .as_ref()
            .and_then(|v| v.reqq)
            .unwrap_or(Session::DEFAULT_REQUEST_QUEUE_LEN);

        if n > 0 {
            self.state.session.target_request_queue_len = n;
        }
    }
}
