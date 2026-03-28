use super::*;
use crate::{
    PEER_BR_MSG_BOUND, TORRENT_MSG_BOUND, magnet::Magnet, metainfo::Info,
};
use bendy::decoding::FromBencode;
use bit_vec::BitVec;

impl Torrent<Connected, FromMagnet> {
    pub(self) async fn inner_run(
        mut self,
    ) -> Result<Option<Torrent<Connected, FromMetaInfo>>, Error> {
        debug!("running torrent: {:?}", self.name);

        self.status = TorrentStatus::DownloadingMetainfo;
        let _ = self.spawn_outbound_peers(false).await;

        loop {
            select! {
                Some(msg) = self.rx.recv() => {
                    match msg {
                        // a torrent from magnet can never answer these msgs.
                        TorrentMsg::Endgame => { }
                        TorrentMsg::Cancel(..) => { }
                        TorrentMsg::DownloadedPiece(_) => { }
                        TorrentMsg::Request { .. } => {}
                        TorrentMsg::WantBlocks(..) => { }
                        TorrentMsg::UnchokeAlgorithm => { }
                        TorrentMsg::GetUnchokedPeers(..) => { }
                        TorrentMsg::OptUnchokeAlgorithm => { }
                        TorrentMsg::PeerHasPieceNotInLocal(..) => { }

                        TorrentMsg::Promote(meta) => {
                            return Ok(Some(self.promote(meta)));
                        },
                        TorrentMsg::BroadcastBlockInfos(sender, reqs, queue) => {
                            self.broadcast_block_infos(sender, reqs, queue);
                        }
                        TorrentMsg::SetTorrentError(code) => {
                            self.status = TorrentStatus::Error(code);
                        }
                        TorrentMsg::GetPeer(peer_id, sender) => {
                            self.get_peer(peer_id, sender);
                        }
                        TorrentMsg::AddIdlePeers(peers) => {
                            self.state.idle_peers.extend(peers);
                        }
                        TorrentMsg::GetTorrentStatus(otx) => {
                            let _ = otx.send(self.status);
                        }
                        TorrentMsg::HaveInfo(tx) => {
                            let _ = tx.send(false);
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
                            return Ok(None);
                        }
                    }
                }
                _ = self.state.heartbeat_interval.tick() => {
                    self.heartbeat_interval().await;
                }
            }
        }
    }

    /// Run the Torrent main event loop to listen to internal [`TorrentMsg`].
    pub async fn run(self) -> Result<(), Error> {
        match self.inner_run().await {
            Ok(o) => match o {
                Some(me) => me.run().await,
                None => Ok(()),
            },
            Err(e) => Err(e),
        }
    }

    async fn downloaded_info_piece(
        &mut self,
        total: usize,
        index: u64,
        bytes: Vec<u8>,
    ) -> Result<(), Error> {
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

        let _info = Info::from_bencode(&info_bytes)?;
        let info = Arc::new(_info.clone());

        // validate the hash of the downloaded info
        // against the hash of the magnet link
        if self.source.magnet.parse_xt_infohash() != info.info_hash {
            warn!("invalid info hash for info: {:?}", info.name);
            self.state.info_pieces.clear();
            return Err(Error::PieceInvalid);
        }

        debug!("name: {:?}", info.name);
        debug!("files: {:?}", info.files);
        debug!("piece_length: {:?}", info.piece_length);
        info!("pieces: {}, blocks: {}", info.pieces(), info.blocks_count(),);

        self.bitfield = Bitfield::from_piece(info.pieces());
        let meta = MetaInfo {
            announce_list: Some(vec![self.source.magnet.trackers().to_vec()]),
            info: _info,
            ..Default::default()
        };
        let meta = Arc::new(meta);
        self.ctx.tx.send(TorrentMsg::Promote(meta.clone())).await?;
        self.ctx.disk_tx.send(DiskMsg::AddTorrent(meta)).await?;
        self.status = TorrentStatus::Downloading;
        let _ = self.ctx.btx.send(PeerBrMsg::HaveInfo);

        Ok(())
    }

    pub(crate) fn promote(
        self,
        meta: Arc<MetaInfo>,
    ) -> Torrent<Connected, FromMetaInfo> {
        self.ctx
            .metadata_size
            .store(meta.info.metadata_size, Ordering::Release);
        Torrent {
            ctx: self.ctx,
            status: self.status,
            config: self.config,
            daemon_ctx: self.daemon_ctx,
            rx: self.rx,
            name: self.name,
            state: self.state,
            source: FromMetaInfo { meta },
            bitfield: self.bitfield,
        }
    }
}

impl Torrent<Idle, FromMagnet> {
    pub fn new_magnet(
        config: Arc<ResolvedConfig>,
        disk_tx: mpsc::Sender<DiskMsg>,
        daemon_ctx: Arc<DaemonCtx>,
        magnet: Magnet,
    ) -> Torrent<Idle, FromMagnet> {
        let (tx, rx) = mpsc::channel::<TorrentMsg>(TORRENT_MSG_BOUND);
        let (btx, _brx) = broadcast::channel::<PeerBrMsg>(PEER_BR_MSG_BOUND);
        let size = magnet.0.length().unwrap_or(u64::MAX);

        let ctx = Arc::new(TorrentCtx {
            counter: Counter::new(),
            disk_size: size.into(),
            btx,
            tx,
            disk_tx,
            info_hash: magnet.parse_xt_infohash(),
            metadata_size: 0.into(),
        });

        Self {
            config,
            bitfield: Bitfield::default(),
            name: magnet.parse_dn(),
            source: FromMagnet { magnet },
            state: Idle {},
            status: TorrentStatus::default(),
            daemon_ctx,
            ctx,
            rx,
        }
    }
}
