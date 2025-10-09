use bendy::encoding::ToBencode;

use crate::extensions::BLOCK_LEN;

use super::*;

impl Torrent<Connected, FromMetaInfo> {
    /// Run the Torrent main event loop to listen to internal [`TorrentMsg`].
    #[tracing::instrument(name = "torrent", skip_all,
        fields(info = ?self.source.info_hash())
    )]
    pub async fn run(&mut self) -> Result<(), Error> {
        debug!("running torrent: {:?}", self.name);

        self.status = TorrentStatus::Downloading;

        let is_seed_only = self.bitfield.count_ones() >= self.bitfield.len();

        if is_seed_only {
            self.status = TorrentStatus::Seeding;
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
                        TorrentMsg::CloneBlockInfosToPeer(qnt, tx) => {
                            self.clone_block_infos_to_peer(qnt, tx).await?;
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
                        TorrentMsg::GetMissingPieces(id, tx) => {
                            let r = self.get_missing_pieces(&id);
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
                        TorrentMsg::PeerConnecting(addr) => {
                            self.state.idle_peers.remove(&addr);
                            // self.state.connecting_peers.push(addr);
                        }
                        TorrentMsg::PeerError(addr) => {
                            self.peer_error(addr).await;
                        },
                        TorrentMsg::PeerConnected(ctx) => {
                            self.peer_connected(ctx).await;
                        }
                        TorrentMsg::Endgame(blocks) => {
                            let _ = self.ctx.btx.send(PeerBrMsg::Endgame(blocks));
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
                _ = self.state.reconnect_interval.tick() => {
                    // self.reconnect_interval().await;
                    let _ = self.spawn_outbound_peers(true).await;
                }
                _ = self.state.log_rates_interval.tick() => {
                    self.log_rates_interval();
                }
                _ = self.state.optimistic_unchoke_interval.tick() => {
                    self.optimistic_unchoke_interval().await;
                }
                // for the unchoke algorithm, the local client is interested in the best
                // uploaders (from their perspctive) (tit-for-tat)
                _ = self.state.unchoke_interval.tick() => {
                    self.unchoke_interval().await;
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

        let (tx, rx) = mpsc::channel::<TorrentMsg>(100);
        let (btx, _brx) = broadcast::channel::<PeerBrMsg>(100);

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
