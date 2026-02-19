use super::*;
use crate::{PEER_MSG_BOUND, TORRENT_MSG_BOUND, extensions::BLOCK_LEN};
use bendy::encoding::ToBencode;

impl Torrent<Connected, FromMetaInfo> {
    /// Run the Torrent main event loop to listen to internal [`TorrentMsg`].
    #[tracing::instrument(name = "torrent", skip_all,
        fields(info = ?self.source.info_hash())
    )]
    pub async fn run(&mut self) -> Result<(), Error> {
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

        self.state.size = self.source.meta_info.info.get_torrent_size() as u64;
        self.state.counter = Counter::from_total_download(
            self.bitfield.count_ones() as u64
                * self.source.meta_info.info.piece_length as u64,
        );

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
                        TorrentMsg::StealBlockInfos(qnt, ctx) => {
                            let _ = self.steal_block_infos(qnt, ctx).await;
                        }
                        TorrentMsg::AddIdlePeers(peers) => {
                            self.state.idle_peers.extend(peers);
                        }
                        TorrentMsg::GetTorrentStatus(otx) => {
                            let _ = otx.send(self.status);
                        }
                        TorrentMsg::GetAnnounceData(otx) => {
                            let downloaded = self.state.counter.total_download();
                            let uploaded = self.state.counter.total_upload();
                            let left = self.state.size.saturating_sub(downloaded);
                            let _ = otx.send((downloaded, uploaded, left));
                        }
                        TorrentMsg::GetAnnounceList(otx) => {
                            let mut v = vec![self.source.meta_info.announce.clone()];

                            if let Some(l) = self.source.meta_info.announce_list.clone() {
                                v.extend(l.into_iter().flatten());
                            }

                            let _ = otx.send(v);
                        }
                        TorrentMsg::PeerHasPieceNotInLocal(id, tx) => {
                            let r = self.peer_has_piece_not_in_local(&id);
                            let _ = tx.send(r);
                        }
                        TorrentMsg::HaveInfo(tx) => {
                            let _ = tx.send(true);
                        }
                        TorrentMsg::GetMetadataSize(tx) => {
                            let m = self.source.meta_info.info.metadata_size;
                            let _ = tx.send(Some(m));
                        }
                        TorrentMsg::GetPeerBitfield(id, tx) => {
                            let bitfield = self.state.peer_pieces.get(&id).cloned();
                            let _ = tx.send(bitfield);
                        }
                        TorrentMsg::SetPeerBitfield(id, bitfield) => {
                            let entry = self.state.peer_pieces.entry(id).or_default();
                            *entry = bitfield;
                        }
                        TorrentMsg::PeerHave(id, piece) => {
                            let bitfield = self.state.peer_pieces.entry(id).or_default();
                            bitfield.safe_set(piece);
                        }
                        TorrentMsg::GetConnectedPeers(otx) => {
                            let _ = otx.send(self.state.connected_peers.clone());
                        }
                        TorrentMsg::ReadPeerByIp(ip, port, otx) => {
                            self.read_peer_by_ip(ip, port, otx);
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
                            return Ok(());
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
                    // self.reconnect_interval().await;
                    let _ = self.spawn_outbound_peers(true).await;
                }
                _ = self.state.log_rates_interval.tick() => {
                    self.log_rates_interval();
                }
                _ = self.state.optimistic_unchoke_interval.tick() => {
                    self.optimistic_unchoke().await;
                }
                // for the unchoke algorithm, the local client is interested in the best
                // uploaders (from their perspctive) (tit-for-tat)
                _ = self.state.unchoke_interval.tick() => {
                    #[cfg(not(feature = "debug"))]
                    self.unchoke().await;
                }
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
        let metadata_size = Some(meta_info.info.metadata_size);

        let (tx, rx) = mpsc::channel::<TorrentMsg>(TORRENT_MSG_BOUND);
        let (btx, _brx) = broadcast::channel::<PeerBrMsg>(PEER_MSG_BOUND);

        let ctx = Arc::new(TorrentCtx {
            free_tx: daemon_ctx.free_tx.clone(),
            btx,
            tx,
            disk_tx,
            info_hash: meta_info.info.info_hash.clone(),
        });

        Self {
            config,
            bitfield,
            source: FromMetaInfo { meta_info },
            state: Idle { metadata_size },
            name,
            status: TorrentStatus::default(),
            daemon_ctx,
            ctx,
            rx,
        }
    }
}
