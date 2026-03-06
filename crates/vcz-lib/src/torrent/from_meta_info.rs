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
                    self.bitfield.count_ones() >= self.bitfield.len();
                if is_seed_only {
                    self.status = TorrentStatus::Seeding;
                }
            }
        }

        {
            let info_bytes = self.source.meta_info.info.to_bencode()?;
            let info_size = self.source.meta_info.info.metadata_size;
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
                        TorrentMsg::Promote(_info) => {
                            // todo
                        }
                        TorrentMsg::Request{peer_ctx, qnt, recipient} => {
                            let pl = self.source.meta_info.info.piece_length;
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
                        TorrentMsg::SendToAllPeers(sender, reqs, queue) => {
                            for p in &self.state.connected_peers {
                                if p.id == sender { continue };
                                let tx = p.tx.clone();
                                let mut blocks = reqs.clone();
                                blocks.extend(queue.clone());
                                tokio::spawn(async move {
                                    let _ = tx.send(PeerMsg::Blocks(blocks)).await;
                                });
                            }
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
                                if p.id == sender { continue };
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
                            let _ =
                                sender.send(
                                    self.state.connected_peers
                                        .iter().find(|p| p.id == peer_id).cloned()
                                );
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
                        TorrentMsg::WantBlocks(qnt, ctx) => {
                            let _ = self.peer_want_blocks(qnt, ctx).await;
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
                        TorrentMsg::SetPeerBitfield(id, bitfield) => {
                            let entry = self.state.peer_pieces.entry(id.clone()).or_default();
                            **entry = bitfield;
                            let _ = self.gen_missing_pieces(id);
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
                        _ => {}
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
        fields(info = ?self.source.meta_info.info.info_hash)
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
}

impl Torrent<Idle, FromMetaInfo> {
    pub fn new_metainfo(
        config: Arc<ResolvedConfig>,
        disk_tx: mpsc::Sender<DiskMsg>,
        daemon_ctx: Arc<DaemonCtx>,
        meta_info: MetaInfo,
        bitfield: Bitfield,
    ) -> Torrent<Idle, FromMetaInfo> {
        let name = meta_info.info.name.clone();
        let metadata_size = meta_info.info.metadata_size.into();

        let (tx, rx) = mpsc::channel::<TorrentMsg>(TORRENT_MSG_BOUND);
        let (btx, _brx) = broadcast::channel::<PeerBrMsg>(PEER_MSG_BOUND);

        let ctx = Arc::new(TorrentCtx {
            size: (meta_info.info.get_torrent_size() as u64).into(),
            counter: Counter::from_total_download(
                bitfield.count_ones() as u64
                    * meta_info.info.piece_length as u64,
            ),
            free_tx: daemon_ctx.free_tx.clone(),
            btx,
            tx,
            disk_tx,
            info_hash: meta_info.info.info_hash.clone(),
            metadata_size,
        });

        Self {
            config,
            bitfield,
            source: FromMetaInfo { meta_info },
            state: Idle {},
            name,
            status: TorrentStatus::default(),
            daemon_ctx,
            ctx,
            rx,
        }
    }
}
