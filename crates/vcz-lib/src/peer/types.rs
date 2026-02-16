use crate::{
    VERSION_PROT,
    bitfield::Reserved,
    counter::Counter,
    daemon::{DaemonCtx, DaemonMsg},
    disk::{DiskMsg, ReturnToDisk},
    error::Error,
    extensions::{
        Core, CoreCodec, Extension, Handshake, HandshakeCodec, MetadataPiece,
        core::BlockInfo,
    },
    peer::{self, RequestManager},
    torrent::{PeerBrMsg, TorrentCtx, TorrentMsg, TorrentStatus},
};
use futures::{
    SinkExt,
    stream::{SplitSink, SplitStream, StreamExt},
};
use rand::{Rng, distr::Alphanumeric};
use rkyv::{Archive, Deserialize, Serialize};
use std::{
    fmt::Display,
    net::SocketAddr,
    ops::{Deref, DerefMut},
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};
use tokio::{
    net::TcpStream,
    sync::{
        mpsc::{self, Receiver},
        oneshot,
    },
    time::timeout,
};
use tokio_util::codec::Framed;
use tracing::debug;

/// The ID of a Peer.
#[derive(
    Clone,
    PartialEq,
    PartialOrd,
    Eq,
    Hash,
    Default,
    Serialize,
    Deserialize,
    Archive,
)]
#[rkyv(compare(PartialEq, PartialOrd), derive(Debug))]
pub struct PeerId(pub [u8; 20]);

pub const DEFAULT_REQUEST_QUEUE_LEN: u16 = 250;

impl PeerId {
    pub fn generate() -> Self {
        let mut peer_id = [0; 20];
        peer_id[..3].copy_from_slice(b"vcz");
        peer_id[3] = b'-';
        // version
        // 0.00.00
        peer_id[4..9].copy_from_slice(VERSION_PROT);
        peer_id[9] = b'-';

        (10..20).for_each(|i| {
            peer_id[i] = rand::rng().sample(Alphanumeric);
        });

        PeerId(peer_id)
    }
}

/// Only used for logging the state of the per in a compact way.
/// am_choking[0], am_interested[1], peer_choking[2], peer_interested[3]
#[derive(PartialEq, Eq)]
pub struct StateLog(pub [char; 4]);

impl Default for StateLog {
    fn default() -> Self {
        StateLog(['-', '-', '-', '-'])
    }
}

impl DerefMut for StateLog {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Deref for StateLog {
    type Target = [char; 4];
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Display for StateLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}{}{}{}", self.0[0], self.0[1], self.0[2], self.0[3],)
    }
}

impl Display for PeerId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", String::from_utf8_lossy(&self.0))
    }
}

impl TryInto<PeerId> for String {
    type Error = String;
    fn try_into(self) -> Result<PeerId, Self::Error> {
        let hash: Vec<u8> = self.into();
        let hash = PeerId::try_from(hash)?;
        Ok(hash)
    }
}

impl std::fmt::Debug for PeerId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = self.to_string();
        f.write_str(&s)
    }
}

impl From<PeerId> for [u8; 20] {
    fn from(value: PeerId) -> Self {
        value.0
    }
}

impl From<PeerId> for String {
    fn from(value: PeerId) -> Self {
        value.to_string()
    }
}

impl From<[u8; 20]> for PeerId {
    fn from(value: [u8; 20]) -> Self {
        Self(value)
    }
}

impl TryFrom<Vec<u8>> for PeerId {
    type Error = &'static str;

    fn try_from(value: Vec<u8>) -> Result<Self, Self::Error> {
        if value.len() != 20 {
            return Err("The PeerId must have exactly 20 bytes");
        }
        let mut buff = [0u8; 20];
        buff[..20].copy_from_slice(&value[..20]);
        Ok(PeerId(buff))
    }
}

/// Ctx that is shared with Torrent and Disk;
#[derive(Debug)]
pub struct PeerCtx {
    pub tx: mpsc::Sender<PeerMsg>,
    pub torrent_ctx: Arc<TorrentCtx>,
    pub direction: Direction,
    pub id: PeerId,

    /// Remote addr of this peer.
    pub remote_addr: SocketAddr,

    /// Our local addr for this peer.
    pub local_addr: SocketAddr,

    /// Counter for upload and download rates, in the local peer perspective.
    pub counter: Counter,

    /// Client is choking the peer.
    pub am_choking: AtomicBool,

    /// Client is interested in downloading from peer.
    pub am_interested: AtomicBool,

    /// The peer is choking the client.
    pub peer_choking: AtomicBool,

    /// The peer is interested in downloading from client.
    pub peer_interested: AtomicBool,
}

/// Messages used to control the peer state or to make the peer forward a
/// message.
#[derive(Debug)]
pub enum PeerMsg {
    /// Tell this peer that we choked them
    Choke,

    /// Tell this peer that we unchoked them
    Unchoke,

    /// Tell this peer that we are interested,
    Interested,

    /// Tell this peer that we are not interested,
    NotInterested,

    /// Send block infos to this peer.
    Blocks(Vec<BlockInfo>),

    CloneBlocks(usize, oneshot::Sender<Vec<BlockInfo>>),

    /// Force the peer to run the interested algorithm immediately.
    InterestedAlgorithm,

    /// Force the peer to run the request algorithm immediately.
    RequestAlgorithm,
}

/// Determines who initiated the connection.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Direction {
    /// Outbound means the local peer initiated the connection
    Outbound,

    /// Inbound means the remote peer initiated the connection
    Inbound,
}

/// A peer can be: Idle, Connected, or Error.
pub trait PeerState {}

/// New peers just returned by the tracker, without any type of connection,
/// ready to be handshaked at any moment.
#[derive(Clone)]
pub struct Idle {}

impl Default for peer::Peer<Idle> {
    fn default() -> Self {
        Self::new()
    }
}

static HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

impl peer::Peer<Idle> {
    pub fn new() -> Self {
        Self { state: Idle {}, state_log: StateLog::default() }
    }

    pub async fn outbound_handshake(
        self,
        socket: TcpStream,
        daemon_ctx: Arc<DaemonCtx>,
        torrent_ctx: Arc<TorrentCtx>,
        metadata_size: Option<usize>,
    ) -> Result<peer::Peer<Connected>, Error> {
        let remote = socket.peer_addr()?;
        let local = socket.local_addr()?;
        let mut socket = Framed::new(socket, HandshakeCodec);
        let info_hash = &torrent_ctx.info_hash;

        tracing::debug!("{remote} sending outbound handshake");

        let mut local_handshake =
            Handshake::new(info_hash.clone(), daemon_ctx.local_peer_id.clone());

        if let Some(ext) = local_handshake.ext.as_mut() {
            ext.metadata_size = Some(metadata_size.unwrap_or(0));
        }

        socket.send(local_handshake.clone()).await?;

        let peer_handshake =
            match timeout(HANDSHAKE_TIMEOUT, socket.next()).await {
                Ok(Some(Ok(handshake))) => handshake,
                Ok(Some(Err(_))) => {
                    return Err(Error::HandshakeInvalid);
                }
                Ok(None) => {
                    tracing::trace!("peer closed connection during handshake");
                    return Err(Error::PeerClosedSocket);
                }
                Err(_) => {
                    tracing::trace!(
                        "handshake timeout with after {}ms",
                        HANDSHAKE_TIMEOUT.as_millis()
                    );
                    return Err(Error::HandshakeTimeout);
                }
            };

        if !peer_handshake.validate(&local_handshake) {
            debug!("handshake is invalid");
            return Err(Error::HandshakeInvalid);
        }

        let socket = socket.map_codec(|_| CoreCodec);

        let (tx, rx) = mpsc::channel::<PeerMsg>(32);

        let ctx = Arc::new(PeerCtx {
            torrent_ctx,
            counter: Counter::default(),
            am_interested: false.into(),
            am_choking: true.into(),
            peer_choking: true.into(),
            peer_interested: false.into(),
            direction: Direction::Outbound,
            remote_addr: remote,
            id: peer_handshake.peer_id,
            tx,
            local_addr: local,
        });

        let _ =
            ctx.torrent_ctx.disk_tx.send(DiskMsg::NewPeer(ctx.clone())).await;
        let _ = ctx.torrent_ctx.btx.send(PeerBrMsg::NewPeer(ctx.clone()));

        let (sink, stream) = socket.split();

        let mut peer = peer::Peer {
            state_log: StateLog::default(),
            state: Connected {
                free_tx: daemon_ctx.free_tx.clone(),
                is_paused: false,
                seed_only: false,
                target_request_queue_len: DEFAULT_REQUEST_QUEUE_LEN,
                ctx,
                extension: None,
                sink,
                stream,
                incoming_requests: Vec::with_capacity(
                    DEFAULT_REQUEST_QUEUE_LEN as usize,
                ),
                req_man_block: RequestManager::new(),
                req_man_meta: RequestManager::new(),
                have_info: false,
                in_endgame: false,
                reserved: peer_handshake.reserved,
                rx,
            },
        };

        if let Some(ext) = peer_handshake.ext {
            peer.handle_ext(ext).await?;
        }

        // send bitfield
        {
            let (otx, orx) = oneshot::channel();
            peer.state
                .ctx
                .torrent_ctx
                .tx
                .send(TorrentMsg::ReadBitfield(otx))
                .await?;

            let bitfield = orx.await?;
            peer.state.sink.send(Core::Bitfield(bitfield)).await?;
        }

        Ok(peer)
    }

    /// Do a handshake (and maybe extended handshake) with the peer and convert
    /// it to a connected peer.
    pub async fn inbound_handshake(
        self,
        socket: TcpStream,
        daemon_ctx: Arc<DaemonCtx>,
    ) -> Result<peer::Peer<Connected>, Error> {
        let remote = socket.peer_addr()?;
        let local = socket.local_addr()?;

        let mut socket = Framed::new(socket, HandshakeCodec);
        let (otx, orx) = oneshot::channel();

        // wait and validate their handshake
        let peer_handshake = match socket.next().await {
            Some(Ok(peer_handshake)) => peer_handshake,
            Some(Err(_)) => {
                // connected to peer but an error happened during the handshake.
                return Err(Error::HandshakeInvalid);
            }
            None => {
                // the peer sent a FIN or resetted the connection.
                // warn!("peer sent FIN.");
                return Err(Error::PeerClosedSocket);
            }
        };

        let mut our_handshake = Handshake::new(
            peer_handshake.info_hash.clone(),
            daemon_ctx.local_peer_id.clone(),
        );

        // in an inbound connection, the client can only know which torrent the
        // peer wants when the peer sends their first handshake, so we send a
        // message to the daemon to get it.
        daemon_ctx
            .tx
            .send(DaemonMsg::GetMetadataSize(
                otx,
                peer_handshake.info_hash.clone(),
            ))
            .await?;

        let metadata_size = orx.await?;

        if let Some(ext) = &mut our_handshake.ext {
            ext.metadata_size = metadata_size;
        }

        if !peer_handshake.validate(&our_handshake) {
            debug!("handshake is invalid");
            return Err(Error::HandshakeInvalid);
        }

        socket.send(our_handshake).await?;

        let (otx, orx) = oneshot::channel();

        daemon_ctx
            .tx
            .send(DaemonMsg::GetTorrentCtx(
                otx,
                peer_handshake.info_hash.clone(),
            ))
            .await?;

        let Some(torrent_ctx) = orx.await? else {
            return Err(Error::TorrentDoesNotExist);
        };

        let (otx, orx) = oneshot::channel();
        torrent_ctx
            .tx
            .send(TorrentMsg::GetPeer(peer_handshake.peer_id.clone(), otx))
            .await?;
        let peer_ctx = orx.await?;

        if peer_ctx.is_some() {
            return Err(Error::NoDuplicatePeer);
        }

        let socket = socket.map_codec(|_| CoreCodec);

        let (tx, rx) = mpsc::channel::<PeerMsg>(32);

        let ctx = PeerCtx {
            torrent_ctx,
            counter: Counter::default(),
            am_interested: false.into(),
            am_choking: true.into(),
            peer_choking: true.into(),
            peer_interested: false.into(),
            direction: Direction::Inbound,
            remote_addr: remote,
            id: peer_handshake.peer_id,
            tx,
            local_addr: local,
        };

        let (sink, stream) = socket.split();

        let mut peer = peer::Peer {
            state_log: StateLog::default(),
            state: Connected {
                free_tx: daemon_ctx.free_tx.clone(),
                is_paused: false,
                seed_only: false,
                target_request_queue_len: DEFAULT_REQUEST_QUEUE_LEN,
                ctx: Arc::new(ctx),
                extension: None,
                sink,
                stream,
                incoming_requests: Vec::with_capacity(50),
                req_man_block: RequestManager::new(),
                req_man_meta: RequestManager::new(),
                have_info: false,
                in_endgame: false,
                reserved: peer_handshake.reserved,
                rx,
            },
        };

        if let Some(ext) = peer_handshake.ext {
            peer.handle_ext(ext).await?;
        }

        // when running a new Peer, we might
        // already have the info downloaded.
        {
            let (otx, orx) = oneshot::channel();
            peer.state
                .ctx
                .torrent_ctx
                .tx
                .send(TorrentMsg::HaveInfo(otx))
                .await?;
            peer.state.have_info = orx.await?;
        }

        {
            let (otx, orx) = oneshot::channel();
            peer.state
                .ctx
                .torrent_ctx
                .tx
                .send(TorrentMsg::GetTorrentStatus(otx))
                .await?;
            peer.state.seed_only = orx.await? == TorrentStatus::Seeding;
        }

        // send bitfield
        {
            let (otx, orx) = oneshot::channel();
            peer.state
                .ctx
                .torrent_ctx
                .tx
                .send(TorrentMsg::ReadBitfield(otx))
                .await?;

            let bitfield = orx.await?;
            peer.state.sink.send(Core::Bitfield(bitfield)).await?;
        }

        Ok(peer)
    }
}

/// Peer is downloading / uploading and working well
pub struct Connected {
    pub stream: SplitStream<Framed<TcpStream, CoreCodec>>,
    pub sink: SplitSink<Framed<TcpStream, CoreCodec>, Core>,
    pub reserved: Reserved,
    pub rx: Receiver<PeerMsg>,
    pub free_tx: mpsc::UnboundedSender<ReturnToDisk>,
    pub extension: Option<Extension>,

    /// Context of the Peer which is shared for anyone who needs it.
    pub ctx: Arc<PeerCtx>,

    /// Our pending requests that we sent to peer. It represents the blocks
    /// that we are expecting.
    ///
    /// If we receive a block whose request entry is here, that entry is
    /// removed. A request is also removed here when it is timed out.
    pub req_man_block: RequestManager<BlockInfo>,

    pub req_man_meta: RequestManager<MetadataPiece>,

    /// The requests we got from peer.
    ///
    /// The request's entry is removed from here when the block is transmitted
    /// or when the peer cancels it. If a peer sends a request and cancels it
    /// before the disk read is done, the read block is dropped.
    pub incoming_requests: Vec<BlockInfo>,

    /// The target request queue size is the number of block requests we keep
    /// outstanding
    pub target_request_queue_len: u16,

    /// This is a cache of have_info on Torrent
    /// to avoid using locks or atomics.
    pub have_info: bool,

    /// Whether we're in endgame mode.
    pub in_endgame: bool,

    /// If the torrent was fully downloaded, all peers will become seed only.
    /// They will only seed but not download anything anymore.
    pub seed_only: bool,

    /// If the client manually paused the local peer, preventing it from
    /// downloading and uploading but keeping connections.
    pub is_paused: bool,
}

/// Tried to do an oubound connection but peer couldn't be reached.
// todo:
// right now a peer is only converted to this state when we try an outbound
// connection and it doesn't work, if the peer is connected and returns an
// error, we should maybe use another type with more information about why it
// failed, such as a peer being malicious.
#[derive(Clone)]
pub struct PeerError {
    pub addr: SocketAddr,
    pub reconnect_attempts: u32,
}

impl peer::Peer<PeerError> {
    pub fn new(addr: SocketAddr) -> Self {
        peer::Peer {
            state: PeerError { addr, reconnect_attempts: 0 },
            state_log: StateLog::default(),
        }
    }
}

impl PeerState for PeerError {}
impl PeerState for Connected {}
impl PeerState for Idle {}
