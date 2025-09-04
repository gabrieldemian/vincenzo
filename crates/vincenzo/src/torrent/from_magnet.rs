use super::*;
use crate::{magnet::Magnet, metainfo::Info};
use bendy::decoding::FromBencode;
use sha1::{Digest, Sha1};

impl Torrent<Connected, FromMagnet> {
    /// Run the Torrent main event loop to listen to internal [`TorrentMsg`].
    #[tracing::instrument(name = "torrent", skip_all,
        fields(info = ?self.source.info_hash())
    )]
    pub async fn run(&mut self) -> Result<(), Error> {
        debug!("running torrent: {:?}", self.name);

        self.spawn_outbound_peers(self.source.info.is_some()).await?;

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
                            // we dont push this addr to the error_peers. If the TCP connection was
                            // made but something happened in the handshake, there is nothing we
                            // can do but to ignore this peer's existence.
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
                            self.quit().await;
                            return Ok(());
                        }
                    }
                }
                _ = self.state.reconnect_interval.tick() => {
                    self.reconnect_interval().await;
                }
                _ = self.state.heartbeat_interval.tick(),
                if self.source.info.is_some() =>
                {
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
                _ = self.state.announce_interval.tick(),
                if self.source.info.is_some() =>
                {
                    self.state.announce_interval = self.announce_interval().await?;
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
        debug!("downloaded_info_piece");

        if self.source.info.is_some() {
            return Ok(());
        };

        if self.status == TorrentStatus::ConnectingTrackers {
            self.status = TorrentStatus::DownloadingMetainfo;
        }

        self.state.info_pieces.entry(index).or_default().extend(bytes);

        let has_info_downloaded =
            self.state.info_pieces.values().fold(0, |acc, v| acc + v.len())
                >= total;

        debug!("has_info_downloaded? {has_info_downloaded}");

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

        let downloaded_info = Info::from_bencode(&info_bytes)?;
        self.state.metadata_size = Some(downloaded_info.size);
        let magnet_hash = self.source.info_hash();

        let mut hasher = Sha1::new();
        hasher.update(&info_bytes);

        let hash = &hasher.finalize()[..];

        // validate the hash of the downloaded info
        // against the hash of the magnet link
        if hex::decode(*magnet_hash).map_err(|_| Error::BencodeError)? != hash {
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

        self.state.size = downloaded_info.get_size();
        let pieces = downloaded_info.pieces();
        self.state.bitfield = Bitfield::from_piece(pieces as usize);

        self.ctx
            .disk_tx
            .send(DiskMsg::NewTorrent(
                self.ctx.clone(),
                downloaded_info.clone(),
            ))
            .await?;

        self.source.info = Some(downloaded_info);
        self.status = TorrentStatus::Downloading;

        let _ = self.ctx.btx.send(PeerBrMsg::HaveInfo);
        Ok(())
    }
}

impl Torrent<Idle, FromMagnet> {
    pub fn new_magnet(
        disk_tx: mpsc::Sender<DiskMsg>,
        free_tx: mpsc::UnboundedSender<ReturnToDisk>,
        daemon_ctx: Arc<DaemonCtx>,
        magnet: Magnet,
    ) -> Torrent<Idle, FromMagnet> {
        let (tx, rx) = mpsc::channel::<TorrentMsg>(100);
        let (btx, _brx) = broadcast::channel::<PeerBrMsg>(500);

        let ctx = Arc::new(TorrentCtx {
            free_tx,
            btx,
            tx: tx.clone(),
            disk_tx,
            info_hash: magnet.parse_xt_infohash(),
        });
        let metadata_size = None;

        Self {
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
