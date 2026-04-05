use super::*;
use crate::{PEER_MSG_BOUND, TORRENT_MSG_BOUND, extensions::BLOCK_LEN};
use bendy::encoding::ToBencode;

impl Torrent<Connected, FromMetaInfo> {
    pub(super) async fn inner_run(
        mut self,
    ) -> Result<Option<Torrent<Connected, FromMetaInfo>>, Error> {
        debug!("running torrent: {:?}", self.name);

        match self.status {
            TorrentStatus::Error(_) => {}
            _ => {
                self.status = TorrentStatus::Downloading;
                let is_seed_only =
                    self.bitfield.count_ones() as usize >= self.bitfield.len();
                if is_seed_only {
                    self.status = TorrentStatus::Seeding;
                }
            }
        }

        {
            let info_bytes = self.source.meta.info.to_bencode()?;
            let info_size = self.source.meta.info.metadata_size;
            let meta_pieces = info_size.div_ceil(BLOCK_LEN);
            let info_pieces = &mut self.state.info_pieces;

            for p in 0..meta_pieces {
                let start = p * BLOCK_LEN;
                let end = (start + BLOCK_LEN).min(info_size);
                info_pieces.insert(p as u64, info_bytes[start..end].into());
            }
        }

        loop {
            select! {
                Some(msg) = self.rx.recv() => {
                    match msg {
                        TorrentMsg::Promote(..) => { }
                        TorrentMsg::DownloadedInfoPiece(..) => {}

                        TorrentMsg::SetPeerBitfield(id, mut bitfield) => {
                            let entry = self.state.peer_pieces.entry(id.clone()).or_default();
                            let client_len = self.bitfield.len();

                            // if the peer sends a bitfield too small
                            bitfield.grow(client_len.saturating_sub(bitfield.len()), false);

                            // when encoding a bitfield, it may not be a multiple of 8
                            bitfield.truncate(client_len);

                            debug_assert!(bitfield.len() >= client_len, "remote peer bitfield must not be smaller than the client bitfield (event loop)");

                            **entry = bitfield;
                            let _ = self.gen_missing_pieces(id);
                        }
                        TorrentMsg::Request{peer_ctx, qnt, recipient} => {
                            let pl = self.source.meta.info.piece_length;
                            let total_bytes = qnt * BLOCK_LEN;
                            let pieces_wanted = total_bytes.div_ceil(pl);

                            let Ok(pieces) = self.get_want_pieces(&peer_ctx.id, pieces_wanted)
                            else {
                                let _ = recipient.send(Ok(vec![]));
                                continue;
                            };
                            let disk_tx = self.ctx.disk_tx.clone();
                            let bitfield = self
                                .state
                                .peer_pieces
                                .get(&peer_ctx.id)
                                .ok_or(Error::PeerDoesNotExist)?
                                .clone();
                            spawn(async move {
                                let _ = disk_tx
                                    .send(DiskMsg::RequestBlocks {
                                        peer_ctx,
                                        pieces,
                                        recipient,
                                        bitfield,
                                    })
                                    .await;
                            });
                        }
                        TorrentMsg::BroadcastBlockInfos(ctx, reqs) => {
                            self.broadcast_block_infos(&ctx.id, reqs);
                        }
                        TorrentMsg::Endgame => {
                            for p in &self.state.connected_peers {
                                let tx = p.tx.clone();
                                tokio::spawn(async move {
                                    let _ = tx.send(PeerMsg::Endgame).await;
                                });
                            }
                        }
                        TorrentMsg::Cancel(sender, block_info) => {
                            for p in &self.state.connected_peers {
                                if p.id == sender.id { continue };
                                let b = block_info.clone();
                                let tx = p.tx.clone();
                                tokio::spawn(async move {
                                    let _ = tx.send(PeerMsg::Cancel(b)).await;
                                });
                            }
                        }
                        TorrentMsg::SetTorrentError(code) => {
                            self.status = TorrentStatus::Error(code);
                        }
                        TorrentMsg::GetPeer(peer_id, sender) => {
                            self.get_peer(peer_id, sender);
                        }
                        TorrentMsg::GetUnchokedPeers(sender) => {
                            let _ = sender.send(self.state.unchoked_peers.clone());
                        }
                        TorrentMsg::UnchokeAlgorithm => {
                            self.unchoke().await;
                        }
                        TorrentMsg::OptUnchokeAlgorithm => {
                            self.optimistic_unchoke().await;
                        }
                        TorrentMsg::AddIdlePeers(peers) => {
                            self.state.idle_peers.extend(peers);
                        }
                        TorrentMsg::GetTorrentStatus(otx) => {
                            let _ = otx.send(self.status);
                        }
                        TorrentMsg::PeerHasPieceNotInLocal(id, tx) => {
                            let r = self.peer_has_piece_not_in_local(&id);
                            let _ = tx.send(r);
                        }
                        TorrentMsg::HaveInfo(tx) => {
                            let _ = tx.send(true);
                        }
                        TorrentMsg::PeerHave(id, piece) => {
                            self.peer_have(id, piece);
                        }
                        TorrentMsg::ReadBitfield(oneshot) => {
                            let _ = oneshot.send(self.bitfield.clone());
                        }
                        TorrentMsg::DownloadedPiece(piece) => {
                            self.downloaded_piece(piece).await;
                        }
                        TorrentMsg::CorruptedPiece(piece) => {
                            self.state.peer_pieces_req.set(piece, false);

                            let torrent_length = self.ctx.disk_size.load(Ordering::Relaxed) as usize;
                            let piece_length = self.source.meta.info.piece_length;

                            for p in &self.state.connected_peers {
                                let tx = p.tx.clone();
                                    let _ = tx.send(PeerMsg::CorruptedPiece{
                                        piece,
                                        torrent_length,
                                        piece_length,
                                    }).await;
                            }
                        }
                        TorrentMsg::PeerError(addr) => {
                            self.peer_error(addr).await;
                        },
                        TorrentMsg::PeerConnected(ctx) => {
                            self.peer_connected(ctx).await;
                        }
                        TorrentMsg::RequestInfoPiece(index, recipient) => {
                            let bytes = self.state.info_pieces.get(&index).cloned();
                            let _ = recipient.send(bytes);
                        }
                        TorrentMsg::TogglePause => {
                            self.toggle_pause();
                        }
                        TorrentMsg::Quit => {
                            self.quit();
                            return Ok(None);
                        }
                    }
                }
                _ = self.state.heartbeat_interval.tick() => {
                    self.heartbeat_interval().await;
                }
                _ = self.state.reconnect_interval.tick(),
                    if !matches!(self.status, TorrentStatus::Error(_)) =>
                {
                    let _ = self.spawn_outbound_peers(true).await;
                }
                _ = self.state.optimistic_unchoke_interval.tick() => {
                    self.optimistic_unchoke().await;
                }
                // for the unchoke algorithm, the local client is interested in the best
                // uploaders (from their perspctive) (tit-for-tat)
                _ = self.state.unchoke_interval.tick() => {
                    #[cfg(not(feature = "integration-test"))]
                    self.unchoke().await;
                }
            }
        }
    }

    /// Run the Torrent main event loop to listen to internal [`TorrentMsg`].
    #[tracing::instrument(name = "torrent", skip_all,
        fields(info = ?self.source.meta.info.info_hash)
    )]
    pub async fn run(self) -> Result<(), Error> {
        match self.inner_run().await {
            Ok(o) => match o {
                Some(me) => return Box::pin(me.run()).await,
                None => Ok(()),
            },
            Err(e) => {
                println!("error running torrent {e:?}");
                Err(e)
            }
        }
    }

    #[inline]
    fn broadcast_block_infos(&self, sender: &PeerId, reqs: Vec<BlockInfo>) {
        for p in &self.state.connected_peers {
            if p.id == *sender {
                continue;
            };
            let tx = p.tx.clone();
            let blocks = reqs.clone();
            tokio::spawn(async move {
                let _ = tx.send(PeerMsg::Blocks(blocks)).await;
            });
        }
    }
}

impl Torrent<Idle, FromMetaInfo> {
    pub fn new_metainfo(
        config: Arc<ResolvedConfig>,
        disk_tx: mpsc::Sender<DiskMsg>,
        daemon_ctx: Arc<DaemonCtx>,
        meta: Arc<MetaInfo>,
        bitfield: Bitfield,
    ) -> Torrent<Idle, FromMetaInfo> {
        let info = &meta.info;
        let name = info.name.clone();
        let metadata_size = info.metadata_size.into();

        let (tx, rx) = mpsc::channel::<TorrentMsg>(TORRENT_MSG_BOUND);
        let (btx, _brx) = broadcast::channel::<PeerBrMsg>(PEER_MSG_BOUND);

        let ctx = Arc::new(TorrentCtx {
            disk_size: (info.get_torrent_size() as u64).into(),
            counter: Counter::from_total_download(
                info.downloaded_bytes(&bitfield) as u64,
            ),
            btx,
            tx,
            disk_tx,
            info_hash: info.info_hash.clone(),
            metadata_size,
        });

        Self {
            config,
            bitfield,
            source: FromMetaInfo { meta },
            state: Idle {},
            name,
            status: TorrentStatus::default(),
            daemon_ctx,
            ctx,
            rx,
        }
    }
}

impl Torrent<Connected, FromMetaInfo> {
    #[inline]
    async fn downloaded_piece(&mut self, piece: usize) {
        tracing::trace!("downloaded_piece {piece}");

        self.bitfield.safe_set(piece, true);

        for diff in self.state.peer_pieces_diff.values_mut() {
            diff.safe_set(piece, false);
        }

        let _ = self.ctx.btx.send(PeerBrMsg::HavePiece(piece));

        let total_pieces = self.bitfield.len();
        let downloaded_pieces = self.bitfield.count_ones();

        let is_download_complete = downloaded_pieces as usize >= total_pieces;

        if !is_download_complete && self.status != TorrentStatus::Seeding {
            return;
        }

        info!("＼(≧▽≦)／ download complete, seeding.");
        self.status = TorrentStatus::Seeding;

        let _ = self
            .state
            .tracker_tx
            .send(TrackerMsg::Announce { event: Event::Completed });

        let _ = self
            .ctx
            .disk_tx
            .send(DiskMsg::FinishedDownload(
                self.source.meta.info.info_hash.clone(),
            ))
            .await;

        let _ = self.ctx.btx.send(PeerBrMsg::Seedonly);

        self.state.peer_pieces_diff.clear();
        self.state.peer_pieces.clear();
    }

    #[inline]
    async fn unchoke(&mut self) {
        let best = self.get_best_interested_downloaders();

        // choke peers no longer in top 3
        for peer in &self.state.unchoked_peers {
            if !best.iter().any(|p| p.id == peer.id) {
                let _ = peer.tx.send(PeerMsg::Choke).await;
            }
        }

        self.state.unchoked_peers = best;

        for peer in &self.state.unchoked_peers {
            if peer.am_choking.load(Ordering::Acquire) {
                let _ = peer.tx.send(PeerMsg::Unchoke).await;
            }
        }
    }

    #[inline]
    pub(crate) fn peer_has_piece_not_in_local(
        &self,
        peer_id: &PeerId,
    ) -> Option<usize> {
        if self.bitfield.count_ones() == 0 {
            return Some(0);
        }
        self.state.peer_pieces_diff.get(peer_id).and_then(|diff| {
            diff.iter().enumerate().find(|(_, b)| *b).map(|(i, _)| i)
        })
    }

    #[inline]
    async fn optimistic_unchoke(&mut self) {
        if let Some(old_opt) = self.state.opt_unchoked_peer.take() {
            // only choke if not in top 3
            if !self.state.unchoked_peers.iter().any(|p| p.id == old_opt.id) {
                let _ = old_opt.tx.send(PeerMsg::Choke).await;
            }
        }

        // select new optimistic unchoke
        if let Some(new_opt) = self.get_random_choked_interested_peer() {
            debug!("optimistically unchoking {:?}", new_opt.id);
            let _ = new_opt.tx.send(PeerMsg::Unchoke).await;
            self.state.opt_unchoked_peer = Some(new_opt);
        }
    }
}
