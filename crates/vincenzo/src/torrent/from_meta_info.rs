use super::*;

impl Torrent<Connected, FromMetaInfo> {
    /// Run the Torrent main event loop to listen to internal [`TorrentMsg`].
    #[tracing::instrument(name = "torrent", skip_all,
        fields(info = ?self.source.info_hash())
    )]
    pub async fn run(&mut self) -> Result<(), Error> {
        debug!("running torrent: {:?}", self.name);

        self.status = TorrentStatus::Downloading;
        self.state.size = self.source.meta_info.info.get_size();
        self.state.bitfield =
            Bitfield::from_piece(self.source.meta_info.info.pieces() as usize);

        // todo: instantiate `self.state.info_pieces` to be able to answer to
        // piece requests from other peers.

        self.spawn_outbound_peers(true).await?;

        loop {
            select! {
                Some(msg) = self.rx.recv() => {
                    match msg {
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
                            let m = self.source.meta_info.info.size;
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
                            let _ = oneshot.send(self.state.bitfield.clone());
                        }
                        TorrentMsg::DownloadedPiece(piece) => {
                            self.downloaded_piece(piece).await;
                        }
                        TorrentMsg::PeerConnecting(addr) => {
                            self.state.idle_peers.retain(|v| *v != addr);
                            self.state.connecting_peers.push(addr);
                        }
                        TorrentMsg::PeerConnectingError(addr) => {
                            self.state.connecting_peers.retain(|v| *v != addr);
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
                            self.quit().await;
                            return Ok(());
                        }
                        _ => {}
                    }
                }
                _ = self.state.reconnect_interval.tick() => {
                    self.reconnect_interval().await;
                }
                _ = self.state.heartbeat_interval.tick() => {
                    self.heartbeat_interval().await;
                }
                _ = self.state.log_rates_interval.tick() => {
                    self.log_rates_interval();
                }
                _ = self.state.optimistic_unchoke_interval.tick() => {
                    self.optimistic_unchoke_interval().await;
                }
                // for the unchoke interval, the local client is interested in the best
                // uploaders (from our perspctive) (tit-for-tat)
                // which gives us the most bytes out of the other
                _ = self.state.unchoke_interval.tick() => {
                    self.unchoke_interval().await;
                }
                _ = self.state.announce_interval.tick() => {
                    self.state.announce_interval = self.announce_interval().await?;
                }
            }
        }
    }
}

impl Torrent<Idle, FromMetaInfo> {
    pub fn new_metainfo(
        disk_tx: mpsc::Sender<DiskMsg>,
        free_tx: mpsc::UnboundedSender<ReturnToDisk>,
        daemon_ctx: Arc<DaemonCtx>,
        meta_info: MetaInfo,
    ) -> Torrent<Idle, FromMetaInfo> {
        let name = meta_info.info.name.clone();
        let metadata_size = Some(meta_info.info.size);

        let (tx, rx) = mpsc::channel::<TorrentMsg>(100);
        let (btx, _brx) = broadcast::channel::<PeerBrMsg>(500);

        let ctx = Arc::new(TorrentCtx {
            free_tx,
            btx,
            tx: tx.clone(),
            disk_tx,
            info_hash: meta_info.info.info_hash.clone(),
        });

        Self {
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
