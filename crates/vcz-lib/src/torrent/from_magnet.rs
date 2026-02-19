use super::*;
use crate::{
    PEER_BR_MSG_BOUND, TORRENT_MSG_BOUND, magnet::Magnet, metainfo::Info,
};
use bendy::decoding::FromBencode;

impl Torrent<Connected, FromMagnet> {
    /// Run the Torrent main event loop to listen to internal [`TorrentMsg`].
    #[tracing::instrument(name = "torrent", skip_all,
        fields(info = ?self.source.info_hash())
    )]
    pub async fn run(&mut self) -> Result<(), Error> {
        debug!("running torrent: {:?}", self.name);

        self.status = TorrentStatus::DownloadingMetainfo;

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
                        TorrentMsg::GetAnnounceData(otx) => {
                            let downloaded = self.state.counter.total_download();
                            let uploaded = self.state.counter.total_upload();
                            let left = self.state.size.saturating_sub(downloaded);
                            let _ = otx.send((downloaded, uploaded, left));
                        }
                        TorrentMsg::GetAnnounceList(otx) => {
                            let _ = otx.send(self.source.magnet.trackers().into());
                        }
                        TorrentMsg::GetTorrentStatus(otx) => {
                            let _ = otx.send(self.status);
                        }
                        TorrentMsg::PeerHasPieceNotInLocal(id, tx) => {
                            let r = self.peer_has_piece_not_in_local(&id);
                            let _ = tx.send(r);
                        }
                        TorrentMsg::HaveInfo(tx) => {
                            let _ = tx.send(self.source.info.is_some());
                        }
                        TorrentMsg::GetMetadataSize(tx) => {
                            let m = self.state.metadata_size;
                            let _ = tx.send(m);
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
                        TorrentMsg::MetadataSize(metadata_size) => {
                            self.metadata_size(metadata_size).await;
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
                        TorrentMsg::DownloadedInfoPiece(total, index, bytes) => {
                            self.downloaded_info_piece(total, index, bytes).await?;
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
                    }
                }
                _ = self.state.heartbeat_interval.tick() => {
                    self.heartbeat_interval().await;
                }
                _ = self.state.reconnect_interval.tick(),
                    if !matches!(self.status, TorrentStatus::Error(_)) =>
                {
                    // self.reconnect_interval().await;
                    let _ = self.spawn_outbound_peers(self.source.info.is_some()).await;
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

    async fn downloaded_info_piece(
        &mut self,
        total: usize,
        index: u64,
        bytes: Vec<u8>,
    ) -> Result<(), Error> {
        if self.source.info.is_some() {
            return Ok(());
        };

        self.state.info_pieces.entry(index).or_default().extend(bytes);

        let has_info_downloaded =
            self.state.info_pieces.values().fold(0, |acc, v| acc + v.len())
                >= total;

        if !has_info_downloaded {
            return Ok(());
        }

        info!("downloaded info_hash");

        // get all the info bytes, in order.
        let info_bytes =
            self.state.info_pieces.values().fold(Vec::new(), |mut acc, b| {
                acc.extend_from_slice(b);
                acc
            });

        let downloaded_info = Arc::new(Info::from_bencode(&info_bytes)?);
        self.state.metadata_size = Some(downloaded_info.metadata_size);

        // validate the hash of the downloaded info
        // against the hash of the magnet link
        if self.source.info_hash() != downloaded_info.info_hash {
            warn!("invalid info hash for info: {:?}", downloaded_info.name);
            self.state.info_pieces.clear();
            return Err(Error::PieceInvalid);
        }

        debug!("name: {:?}", downloaded_info.name);
        debug!("files: {:?}", downloaded_info.files);
        debug!("piece_length: {:?}", downloaded_info.piece_length);
        info!(
            "pieces: {}, blocks: {}",
            downloaded_info.pieces(),
            downloaded_info.blocks_count(),
        );

        self.state.size = downloaded_info.get_torrent_size() as u64;
        self.bitfield = Bitfield::from_piece(downloaded_info.pieces());

        self.ctx
            .disk_tx
            .send(DiskMsg::AddTorrent(
                self.ctx.clone(),
                downloaded_info.clone(),
            ))
            .await?;

        // todo: not really using self.source.info for anything except to return
        // a boolean.
        self.source.info = Some(downloaded_info);
        self.status = TorrentStatus::Downloading;

        let _ = self.ctx.btx.send(PeerBrMsg::HaveInfo);
        Ok(())
    }
}

impl Torrent<Idle, FromMagnet> {
    pub fn new_magnet(
        config: Arc<ResolvedConfig>,
        disk_tx: mpsc::Sender<DiskMsg>,
        free_tx: mpsc::UnboundedSender<ReturnToDisk>,
        daemon_ctx: Arc<DaemonCtx>,
        magnet: Magnet,
    ) -> Torrent<Idle, FromMagnet> {
        let (tx, rx) = mpsc::channel::<TorrentMsg>(TORRENT_MSG_BOUND);
        let (btx, _brx) = broadcast::channel::<PeerBrMsg>(PEER_BR_MSG_BOUND);

        let ctx = Arc::new(TorrentCtx {
            free_tx,
            btx,
            tx,
            disk_tx,
            info_hash: magnet.parse_xt_infohash(),
        });
        let metadata_size = None;

        Self {
            config,
            bitfield: Bitfield::default(),
            name: magnet.parse_dn(),
            source: FromMagnet { magnet, info: None },
            state: Idle { metadata_size },
            status: TorrentStatus::default(),
            daemon_ctx,
            ctx,
            rx,
        }
    }
}
