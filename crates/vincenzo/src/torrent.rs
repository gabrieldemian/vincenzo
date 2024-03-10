//! Torrent that is spawned by the Daemon
use crate::{
    bitfield::Bitfield, daemon::DaemonMsg, disk::DiskMsg, error::Error, magnet::Magnet, metainfo::Info, peer::{
        session::ConnectionState, Direction, Peer, PeerCtx, PeerId, PeerMsg
    }, tcp_wire::BlockInfo, tracker::{event::Event, Tracker, TrackerCtx, TrackerMsg}
};
use bendy::decoding::FromBencode;
use bitvec::{bitvec, prelude::Msb0};
use hashbrown::HashMap;
use speedy::{Readable, Writable};
use std::{
    collections::BTreeMap, net::SocketAddr, ops::{Deref, DerefMut}, sync::Arc, time::Duration
};
use tokio::{
    net::{TcpListener, TcpStream}, select, spawn, sync::{mpsc, oneshot, RwLock}, time::{interval, interval_at, timeout, Instant}
};
use tracing::{debug, info, warn};

#[derive(Debug, Clone, Default, Hash, PartialEq, Eq, Writable, Readable)]
pub struct InfoHash(pub [u8; 20]);

impl From<[u8; 20]> for InfoHash {
    fn from(value: [u8; 20]) -> Self {
        Self(value)
    }
}

impl Deref for InfoHash {
    type Target = [u8; 20];
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for InfoHash {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

#[derive(Debug)]
pub enum TorrentMsg {
    /// Message when one of the peers have downloaded
    /// an entire piece. We send Have messages to peers
    /// that don't have it and update the UI with stats.
    DownloadedPiece(usize),
    /// The Peer will send this message if it manages to succesfully handshake
    /// and connect.
    PeerConnected(PeerId, Arc<PeerCtx>),
    DownloadComplete,
    /// When in endgame mode, the first peer that receives this info,
    /// sends this message to send Cancel's to all other peers.
    SendCancelBlock {
        from: PeerId,
        block_info: BlockInfo,
    },
    /// When a peer downloads a piece of a metadata,
    /// send cancels to all other peers so that we dont receive
    /// pieces that we already have
    SendCancelMetadata {
        from: PeerId,
        index: u32,
    },
    StartEndgame(PeerId, Vec<BlockInfo>),
    /// When a peer downloads an info piece,
    /// we need to mutate `info_dict` and maybe
    /// generate the entire info.
    /// total, metadata.index, bytes
    DownloadedInfoPiece(u32, u32, Vec<u8>),
    /// When a peer request a piece of the info
    /// index, recipient
    RequestInfoPiece(u32, oneshot::Sender<Option<Vec<u8>>>),
    IncrementDownloaded(u32),
    IncrementUploaded(u32),
    /// Toggle pause torrent and send Pause/Resume message to all Peers
    TogglePause,
    /// When we can't do a TCP connection with the ip of the Peer.
    FailedPeer(SocketAddr),
    /// When torrent is being gracefully shutdown
    Quit,
}

/// This is the main entity responsible for the high-level management of
/// a torrent download or upload.
#[derive(Debug)]
pub struct Torrent {
    pub ctx: Arc<TorrentCtx>,
    pub tracker_ctx: Arc<TrackerCtx>,
    /// key: peer_id
    pub peer_ctxs: HashMap<PeerId, Arc<PeerCtx>>,
    pub failed_peers: Vec<SocketAddr>,
    /// If using a Magnet link, the info will be downloaded in pieces
    /// and those pieces may come in different order,
    /// hence the HashMap (dictionary), and not a vec.
    /// After the dict is complete, it will be decoded into [`info`]
    pub info_pieces: BTreeMap<u32, Vec<u8>>,
    /// How many bytes we have uploaded to other peers.
    pub uploaded: u64,
    /// How many bytes we have downloaded from other peers.
    pub downloaded: u64,
    pub daemon_tx: mpsc::Sender<DaemonMsg>,
    pub status: TorrentStatus,
    /// Stats of the current Torrent, returned from tracker on announce
    /// requests.
    pub stats: Stats,
    /// The downloaded bytes of the previous second,
    /// used to get the download rate in seconds.
    /// this will be mutated on the frontend event loop.
    pub last_second_downloaded: u64,
    /// The download rate of the torrent, in bytes
    pub download_rate: u64,
    /// The total size of the torrent files, in bytes,
    /// this is a cache of ctx.info.get_size()
    // pub size: u64,
    pub name: String,
    active_peers_count: u8,
    rx: mpsc::Receiver<TorrentMsg>,
}

/// Context of [`Torrent`] that can be shared between other types
#[derive(Debug)]
pub struct TorrentCtx {
    pub disk_tx: mpsc::Sender<DiskMsg>,
    pub tx: mpsc::Sender<TorrentMsg>,
    pub magnet: Magnet,
    pub info_hash: InfoHash,
    pub bitfield: RwLock<Bitfield>,
    pub info: RwLock<Info>,
    pub have_info: RwLock<bool>,
}

/// State of a [`Torrent`], used by the UI to present data.
#[derive(Debug, Clone, Default, PartialEq, Readable, Writable)]
pub struct TorrentState {
    pub name: String,
    pub stats: Stats,
    pub status: TorrentStatus,
    pub downloaded: u64,
    pub download_rate: u64,
    pub uploaded: u64,
    pub size: u64,
    pub info_hash: InfoHash,
}

/// Status of the current Torrent, updated at every announce request.
#[derive(Clone, Debug, PartialEq, Default, Readable, Writable)]
pub struct Stats {
    pub interval: u32,
    pub leechers: u32,
    pub seeders: u32,
}

impl Torrent {
    const MAX_ACTIVE_PEERS_LEN: u8 = 10;

    #[tracing::instrument(skip(disk_tx, daemon_tx), name = "torrent::new")]
    pub fn new(
        disk_tx: mpsc::Sender<DiskMsg>,
        daemon_tx: mpsc::Sender<DaemonMsg>,
        magnet: Magnet,
    ) -> Self {
        let name = magnet.parse_dn();
        let bitfield = RwLock::new(Bitfield::default());
        let info = RwLock::new(Info::default().name(name.clone()));
        let info_pieces = BTreeMap::new();
        let tracker_ctx = Arc::new(TrackerCtx::default());

        let (tx, rx) = mpsc::channel::<TorrentMsg>(300);

        let ctx = Arc::new(TorrentCtx {
            tx: tx.clone(),
            disk_tx,
            info_hash: magnet.parse_xt(),
            have_info: RwLock::new(false),
            bitfield,
            magnet,
            info,
        });

        Self {
            name,
            active_peers_count: 0,
            // size: 0,
            last_second_downloaded: 0,
            download_rate: 0,
            status: TorrentStatus::default(),
            stats: Stats::default(),
            daemon_tx,
            uploaded: 0,
            downloaded: 0,
            info_pieces,
            tracker_ctx,
            ctx,
            rx,
            peer_ctxs: HashMap::new(),
            failed_peers: Vec::new(),
        }
    }

    /// Start the Torrent and run the tracker, by sending `connect` and
    /// `announce_exchange` messages to one of the trackers, and returning a
    /// list of peers.
    #[tracing::instrument(skip(self), name = "torrent::start")]
    pub async fn start(
        &mut self,
        listen: Option<SocketAddr>,
    ) -> Result<Vec<SocketAddr>, Error> {
        let mut tracker =
            Tracker::connect(self.ctx.magnet.parse_trackers()).await?;
        let info_hash = self.ctx.clone().info_hash.clone();
        let (res, peers) = tracker.announce_exchange(info_hash, listen).await?;

        self.stats = Stats {
            interval: res.interval,
            seeders: res.seeders,
            leechers: res.leechers,
        };

        debug!(
            "starting torrent {:?} with stats {:#?}",
            self.ctx.info_hash, self.stats
        );

        self.tracker_ctx = tracker.ctx.clone().into();

        spawn(async move {
            tracker.run().await?;
            Ok::<(), Error>(())
        });

        Ok(peers)
    }

    /// Start the Torrent, run its Tracker,
    /// and spawns all the event loops.
    pub async fn start_and_run(
        &mut self,
        listen: Option<SocketAddr>,
    ) -> Result<(), Error> {
        let peers = self.start(listen).await?;
        self.spawn_outbound_peers(peers).await?;
        self.spawn_inbound_peers().await?;
        self.run().await?;
        Ok(())
    }

    /// Start the Torrent with the given peers,
    /// and spawns all the event loops.
    pub async fn start_and_run_with_peers(
        &mut self,
        peers: Vec<SocketAddr>,
    ) -> Result<(), Error> {
        // let peers = self.start(listen).await?;
        self.spawn_outbound_peers(peers).await?;
        self.spawn_inbound_peers().await?;
        self.run().await?;
        Ok(())
    }

    /// Spawn an event loop for each peer
    #[tracing::instrument(skip_all)]
    pub async fn spawn_outbound_peers(
        &mut self,
        peers: Vec<SocketAddr>,
    ) -> Result<(), Error> {
        debug!(
            "there are {} peers. Will try to connect up to {}",
            peers.len(),
            Self::MAX_ACTIVE_PEERS_LEN
        );

        for peer in peers {
            if self.active_peers_count >= Self::MAX_ACTIVE_PEERS_LEN {
                return Ok(());
            }
            let ctx = self.ctx.clone();
            let local_peer_id = self.tracker_ctx.peer_id.clone();
            let connection_fut = TcpStream::connect(peer);

            // send connections too other peers
            spawn(async move {
                if let Ok(r) =
                    timeout(Duration::from_secs(3), connection_fut).await
                {
                    match r {
                        Ok(socket) => {
                            info!("connected with peer: {socket:?}",);
                            let p = Self::start_and_run_peer(
                                ctx,
                                socket,
                                local_peer_id,
                                Direction::Outbound,
                            )
                            .await?;
                        }
                        Err(e) => {
                            warn!(
                                "could not connect to peer: {:?} {}",
                                peer,
                                e.to_string()
                            );
                            ctx.tx.send(TorrentMsg::FailedPeer(peer)).await?;
                        }
                    }
                }
                Ok::<(), Error>(())
            });
        }
        Ok(())
    }

    async fn start_and_run_peer(
        ctx: Arc<TorrentCtx>,
        socket: TcpStream,
        local_peer_id: PeerId,
        direction: Direction,
    ) -> Result<Peer, Error> {
        let (socket, handshake) = Peer::handshake(
            socket,
            direction,
            ctx.info_hash.clone(),
            local_peer_id,
        )
        .await?;

        let local = socket.get_ref().local_addr()?;
        let remote = socket.get_ref().peer_addr()?;

        let mut peer = Peer::new(remote, ctx, handshake, local);
        let r = peer.run(direction, socket).await;
        // if we are gracefully shutting down, we do nothing with the pending
        // blocks, Rust will drop them when their scope ends naturally.
        // otherwise, we send the blocks back to the torrent
        // so that other peers can download them. In this case, the peer
        // might be shutting down due to an error or this is malicious peer
        // that we wish to end the connection.
        if peer.session.state.connection != ConnectionState::Quitting {
            peer.free_pending_blocks().await;
        }

        if let Err(r) = r {
            debug!(
                "{} peer session stopped due to an error: {}",
                peer.ctx.remote_addr, r
            );
            return Err(r);
        }

        Ok(peer)
    }

    /// Spawn a new event loop every time a peer connect with us.
    #[tracing::instrument(skip_all)]
    pub async fn spawn_inbound_peers(&self) -> Result<(), Error> {
        let local_peer_socket =
            TcpListener::bind(self.tracker_ctx.local_peer_addr).await?;
        let local_peer_id = self.tracker_ctx.peer_id.clone();
        let ctx = self.ctx.clone();

        // accept connections from other peers
        spawn(async move {
            debug!(
                "accepting requests in {:?}",
                local_peer_socket.local_addr()
            );

            loop {
                let local_peer_id = local_peer_id.clone();
                if let Ok((socket, addr)) = local_peer_socket.accept().await {
                    info!("received inbound connection from {addr}");
                    let ctx = ctx.clone();

                    spawn(async move {
                        Self::start_and_run_peer(
                            ctx,
                            socket,
                            local_peer_id,
                            Direction::Inbound,
                        )
                        .await?;
                        Ok::<(), Error>(())
                    });
                }
            }
        });

        Ok(())
    }

    /// Run the Torrent main event loop to listen to internal [`TorrentMsg`].
    #[tracing::instrument(skip_all)]
    pub async fn run(&mut self) -> Result<(), Error> {
        debug!("running torrent: {:?}", self.name);

        let tracker_tx = self.tracker_ctx.tx.clone();

        let mut announce_interval = interval_at(
            Instant::now()
                + Duration::from_secs(self.stats.interval.max(500).into()),
            Duration::from_secs((self.stats.interval as u64).max(500)),
        );

        let mut reconnect_failed_peers = interval_at(
            Instant::now() + Duration::from_secs(5),
            Duration::from_secs(5),
        );

        let mut frontend_interval = interval(Duration::from_secs(1));

        loop {
            select! {
                Some(msg) = self.rx.recv() => {
                    match msg {
                        TorrentMsg::DownloadedPiece(piece) => {
                            // send Have messages to peers that dont have our pieces
                            for peer in self.peer_ctxs.values() {
                                let _ = peer.tx.send(PeerMsg::HavePiece(piece)).await;
                            }
                        }
                        TorrentMsg::PeerConnected(id, ctx) => {
                            self.peer_ctxs.insert(id.clone(), ctx.clone());
                            self.active_peers_count += 1;

                            let _ = self
                                .ctx
                                .disk_tx
                                .send(DiskMsg::NewPeer(ctx.clone()))
                                .await;

                            // let have_info = self.ctx.have_info.read().await;

                            let _ = self.daemon_tx.send(DaemonMsg::PeerConnected(id.clone(), ctx)).await;

                            // debug!("connected with peer of id {:?} have_info? {}", *id.to_string(), have_info);
                        }
                        TorrentMsg::DownloadComplete => {
                            info!("Downloaded torrent {:?}", self.name);
                            let (otx, orx) = oneshot::channel();

                            self.status = TorrentStatus::Seeding;

                            if let Some(tracker_tx) = &tracker_tx {
                                let _ = tracker_tx.send(
                                    TrackerMsg::Announce {
                                        event: Event::Completed,
                                        info_hash: self.ctx.info_hash.clone(),
                                        downloaded: self.downloaded,
                                        uploaded: self.uploaded,
                                        left: 0,
                                        recipient: Some(otx),
                                    })
                                .await;
                            }

                            if let Ok(Ok(r)) = orx.await {
                                debug!("announced completion with success {r:#?}");
                                self.stats = r.into();
                            }

                            // tell all peers that we are not interested,
                            // we wont request blocks from them anymore
                            for peer in self.peer_ctxs.values() {
                                let _ = peer.tx.send(PeerMsg::NotInterested).await;
                                let _ = peer.tx.send(PeerMsg::SeedOnly).await;
                            }
                        }
                        // The peer "from" was the first one to receive the "info".
                        // Send Cancel messages to everyone else.
                        TorrentMsg::SendCancelBlock { from, block_info } => {
                            for (k, peer) in self.peer_ctxs.iter() {
                                if *k == from { continue };
                                let _ = peer.tx.send(PeerMsg::CancelBlock(block_info.clone())).await;
                            }
                        }
                        TorrentMsg::SendCancelMetadata { from, index } => {
                            debug!("received SendCancelMetadata {from:?} {index}");
                            for (k, peer) in self.peer_ctxs.iter() {
                                if *k == from { continue };
                                let _ = peer.tx.send(PeerMsg::CancelMetadata(index)).await;
                            }
                        }
                        TorrentMsg::StartEndgame(_peer_id, block_infos) => {
                            info!("Started endgame mode for {}", self.name);
                            for (_id, peer) in self.peer_ctxs.iter() {
                                let _ = peer.tx.send(PeerMsg::RequestBlockInfos(block_infos.clone())).await;
                            }
                        }
                        TorrentMsg::DownloadedInfoPiece(total, index, bytes) => {
                            debug!("received DownloadedInfoPiece");

                            if self.status == TorrentStatus::ConnectingTrackers {
                                self.status = TorrentStatus::DownloadingMetainfo;
                            }

                            self.info_pieces.insert(index, bytes);

                            let info_len = self.info_pieces.values().fold(0, |acc, b| {
                                acc + b.len()
                            });

                            let have_all_pieces = info_len as u32 >= total;

                            let have_info = self.ctx.have_info.read().await.clone();
                            if have_all_pieces && !have_info {
                                // info has a valid bencode format
                                let info_bytes = self.info_pieces.values().fold(Vec::new(), |mut acc, b| {
                                    acc.extend_from_slice(b);
                                    acc
                                });
                                let info = Info::from_bencode(&info_bytes).map_err(|_| Error::BencodeError)?;

                                let m_info = self.ctx.magnet.xt.clone().unwrap();

                                let mut hash = sha1_smol::Sha1::new();
                                hash.update(&info_bytes);

                                let hash = hash.digest().bytes();

                                // validate the hash of the downloaded info
                                // against the hash of the magnet link
                                let hash = hex::encode(hash);

                                if hash.to_uppercase() == m_info.to_uppercase() {
                                    debug!("the hash of the downloaded info matches the hash of the magnet link");

                                    // with the info fully downloaded, we now know the pieces len,
                                    // this will update the bitfield of the torrent
                                    let mut bitfield = self.ctx.bitfield.write().await;
                                    *bitfield = bitvec![u8, Msb0; 0; info.pieces() as usize];

                                    // remove excess bits
                                    if (info.pieces() as usize) < bitfield.len() {
                                        unsafe {
                                            bitfield.set_len(info.pieces() as usize);
                                        }
                                    }

                                    debug!("local_bitfield is now of len {:?}", bitfield.len());

                                    // self.size = info.get_size();
                                    let mut have_info = self.ctx.have_info.write().await;
                                    *have_info = true;
                                    drop(have_info);

                                    let mut info_l = self.ctx.info.write().await;

                                    debug!("new info piece length {:?}", info.piece_length);
                                    debug!("new info pieces_len {:?}", info.pieces.len());
                                    debug!("new info pieces_len {:?}", info.pieces.len());
                                    debug!("new info file_length {:?}", info.file_length);
                                    debug!("new info files {:#?}", info.files);

                                    *info_l = info;
                                    drop(info_l);

                                    self.status = TorrentStatus::Downloading;
                                    self.ctx.disk_tx.send(DiskMsg::NewTorrent(self.ctx.clone())).await?;
                                } else {
                                    warn!("a peer sent a valid Info, but the hash does not match the hash of the provided magnet link, panicking");
                                    return Err(Error::PieceInvalid);
                                }
                            }
                        }
                        TorrentMsg::RequestInfoPiece(index, recipient) => {
                            debug!("received RequestInfoPiece {index}");
                            let bytes = self.info_pieces.get(&index).cloned();
                            let _ = recipient.send(bytes);
                        }
                        TorrentMsg::IncrementDownloaded(n) => {
                            self.downloaded += n as u64;

                            // check if the torrent download is complete
                            let size = self.ctx.info.read().await.get_size();
                            let is_download_complete = self.downloaded >= size;
                            debug!("IncrementDownloaded {:?}", self.downloaded);
                            debug!("size is {}", size);

                            if is_download_complete {
                                self.ctx.tx.send(TorrentMsg::DownloadComplete).await?;
                            }
                        }
                        TorrentMsg::IncrementUploaded(n) => {
                            self.uploaded += n as u64;
                            debug!("IncrementUploaded {}", self.uploaded);
                        }
                        TorrentMsg::TogglePause => {
                            debug!("torrent TogglePause");
                            // can only pause if the torrent is not connecting, or not erroring
                            if self.status == TorrentStatus::Downloading || self.status == TorrentStatus::Seeding || self.status == TorrentStatus::Paused {
                                info!("Paused torrent {:?}", self.name);
                                if self.status == TorrentStatus::Paused {
                                    let size = self.ctx.info.read().await.get_size();
                                    if self.downloaded >= size {
                                        self.status = TorrentStatus::Seeding;
                                    } else {
                                        self.status = TorrentStatus::Downloading;
                                    }
                                } else {
                                    self.status = TorrentStatus::Paused;
                                }
                                for (_, peer) in &self.peer_ctxs {
                                    if self.status == TorrentStatus::Paused {
                                        let _ = peer.tx.send(PeerMsg::Pause).await;
                                    } else {
                                        let _ = peer.tx.send(PeerMsg::Resume).await;
                                    }
                                }
                            }
                        }
                        TorrentMsg::FailedPeer(addr) => {
                            self.failed_peers.push(addr);
                        },
                        TorrentMsg::Quit => {
                            info!("Quitting torrent {:?}", self.name);
                            let (otx, orx) = oneshot::channel();
                            let info = self.ctx.info.read().await;
                            let left =
                                if self.downloaded > info.get_size()
                                    { self.downloaded - info.get_size() }
                                else { 0 };

                            if let Some(tracker_tx) = &tracker_tx {
                                let _ = tracker_tx.send(
                                    TrackerMsg::Announce {
                                        event: Event::Stopped,
                                        info_hash: self.ctx.info_hash.clone(),
                                        downloaded: self.downloaded,
                                        uploaded: self.uploaded,
                                        left,
                                        recipient: Some(otx),
                                    })
                                .await;
                            }

                            for peer in self.peer_ctxs.values() {
                                let tx = peer.tx.clone();
                                spawn(async move {
                                    let _ = tx.send(PeerMsg::Quit).await;
                                });
                            }

                            orx.await??;

                            return Ok(());
                        }
                    }
                }
                _ = frontend_interval.tick() => {
                    self.download_rate = self.downloaded - self.last_second_downloaded;
                    let size = self.ctx.info.read().await.get_size();

                    let torrent_state = TorrentState {
                        name: self.name.clone(),
                        size,
                        downloaded: self.downloaded,
                        uploaded: self.uploaded,
                        stats: self.stats.clone(),
                        status: self.status.clone(),
                        download_rate: self.download_rate,
                        info_hash: self.ctx.info_hash.clone(),
                    };

                    self.last_second_downloaded = self.downloaded;
                    // debug!(
                    //     "{} {} of {}. Download rate: {}",
                    //     self.name,
                    //     to_human_readable(self.size as f64),
                    //     to_human_readable(self.downloaded as f64),
                    //     to_human_readable(self.download_rate as f64),
                    // );

                    // send updated information to daemon
                    let _ = self.daemon_tx.send(DaemonMsg::TorrentState(torrent_state)).await;
                }
                // periodically announce to tracker, at the specified interval
                // to update the tracker about the client's stats.
                _ = announce_interval.tick() => {
                    let info = self.ctx.info.read().await;

                    // we know if the info is downloaded if the piece_length is > 0
                    if info.piece_length > 0 {
                        debug!("sending periodic announce, interval {announce_interval:?}");
                        let left = if self.downloaded < info.get_size() { info.get_size() } else { self.downloaded - info.get_size() };

                        let (otx, orx) = oneshot::channel();

                        if let Some(tracker_tx) = &tracker_tx {
                            let _ = tracker_tx.send(
                                TrackerMsg::Announce {
                                    event: Event::None,
                                    info_hash: self.ctx.info_hash.clone(),
                                    downloaded: self.downloaded,
                                    uploaded: self.uploaded,
                                    left,
                                    recipient: Some(otx),
                                })
                            .await;
                        }

                        let r = orx.await??;
                        debug!("new stats {r:#?}");

                        // update our stats, received from the tracker
                        self.stats = r.into();

                        announce_interval = interval(
                            Duration::from_secs(self.stats.interval as u64),
                        );
                    }
                    drop(info);
                }
                // At every 5 seconds, try to reconnect to peers in which
                // the TCP connection failed.
                _ = reconnect_failed_peers.tick() => {
                    for peer in self.failed_peers.clone() {
                        let ctx = self.ctx.clone();
                        let local_peer_id = self.tracker_ctx.peer_id.clone();

                        if let Ok(socket) = TcpStream::connect(peer).await {
                            self.failed_peers.retain(|v| *v != peer);
                            Self::start_and_run_peer(ctx, socket, local_peer_id, Direction::Outbound)
                                .await?;
                        }
                    }
                }
            }
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Readable, Writable)]
pub enum TorrentStatus {
    #[default]
    ConnectingTrackers,
    DownloadingMetainfo,
    Downloading,
    Seeding,
    Paused,
    Error,
}

impl<'a> From<TorrentStatus> for &'a str {
    fn from(val: TorrentStatus) -> Self {
        use TorrentStatus::*;
        match val {
            ConnectingTrackers => "Connecting to trackers",
            DownloadingMetainfo => "Downloading metainfo",
            Downloading => "Downloading",
            Seeding => "Seeding",
            Paused => "Paused",
            Error => "Error",
        }
    }
}

impl From<TorrentStatus> for String {
    fn from(val: TorrentStatus) -> Self {
        use TorrentStatus::*;
        match val {
            ConnectingTrackers => "Connecting to trackers".to_owned(),
            DownloadingMetainfo => "Downloading metainfo".to_owned(),
            Downloading => "Downloading".to_owned(),
            Seeding => "Seeding".to_owned(),
            Paused => "Paused".to_owned(),
            Error => "Error".to_owned(),
        }
    }
}

impl From<&str> for TorrentStatus {
    fn from(value: &str) -> Self {
        use TorrentStatus::*;
        match value {
            "Connecting to trackers" => ConnectingTrackers,
            "Downloading metainfo" => DownloadingMetainfo,
            "Downloading" => Downloading,
            "Seeding" => Seeding,
            "Paused" => Paused,
            _ => Error,
        }
    }
}
