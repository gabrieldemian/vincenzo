use std::{
    fmt::Display,
    net::SocketAddr,
    ops::{Deref, DerefMut},
    sync::{
        atomic::{AtomicBool, AtomicU64},
        Arc,
    },
    time::Duration,
};

use bitvec::{array::BitArray, order::Msb0};
use futures::{
    stream::{SplitSink, SplitStream, StreamExt},
    SinkExt,
};
use rand::{distr::Alphanumeric, Rng};
use speedy::{Readable, Writable};
use tokio::{
    net::TcpStream,
    sync::{
        mpsc::{self, Receiver},
        oneshot, Mutex,
    },
    time::{sleep, timeout, Instant},
};
use tokio_util::codec::{Framed, FramedParts};
use tracing::{debug, warn};

use crate::{
    bitfield::{Bitfield, Reserved},
    counter::Counter,
    daemon::{DaemonCtx, DaemonMsg},
    error::Error,
    extensions::{
        core::BlockInfo, Core, CoreCodec, Extension, Handshake, HandshakeCodec,
        HolepunchData, MetadataData,
    },
    peer::{self},
    torrent::{InfoHash, TorrentCtx, TorrentMsg},
};

/// At any given time, a connection with a handshaked(connected) peer has 3
/// possible states. ConnectionState means TCP connection, Even if the peer is
/// choked they are still marked here as connected.
#[derive(Clone, Default, Copy, Debug, PartialEq)]
pub enum ConnectionState {
    /// The handshake just happened, probably computing choke algorithm and
    /// sending bitfield messages.
    #[default]
    Connecting,

    // Connected and downloading and uploading.
    Connected,

    /// This state is set when the program is gracefully shutting down,
    /// In this state, we don't send the outgoing blocks to the tracker on
    /// shutdown.
    Quitting,
}

#[derive(Clone, PartialEq, Eq, Hash, Default, Readable, Writable)]
pub struct PeerId(pub [u8; 20]);

pub const DEFAULT_REQUEST_QUEUE_LEN: u16 = 200;

impl PeerId {
    pub fn gen() -> Self {
        let mut peer_id = [0; 20];
        peer_id[..3].copy_from_slice(b"vcz");
        peer_id[3] = b'-';
        // 0.00.01
        peer_id[4..9].copy_from_slice(b"00001");
        peer_id[9] = b'-';

        for i in 10..20 {
            peer_id[i] = rand::rng().sample(Alphanumeric);
        }

        PeerId(peer_id)
    }
}

/// Only used for logging the state of the per in a compact way.
/// am_choking, am_interested, peer_choking, peer_interested
pub(crate) struct StateLog(pub [char; 4]);

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
    /// Who initiated the connection, the local peer or remote.
    pub direction: Direction,

    pub tx: mpsc::Sender<PeerMsg>,

    /// Id of the remote peer.
    pub id: PeerId,

    /// Remote addr of this peer.
    pub remote_addr: SocketAddr,

    /// Our local addr for this peer.
    pub local_addr: SocketAddr,

    /// The info_hash of the torrent that this Peer belongs to.
    pub info_hash: InfoHash,

    /// Download bytes of remote peer in the previous 10 seconds, tracked by
    /// the torrent.
    pub downloaded: AtomicU64,

    /// Counter for upload and download rates, in the local peer perspective.
    pub counter: Counter,

    pub last_download_rate_update: Mutex<Instant>,

    /// Upload bytes of remote peer in the previous 10 seconds, tracked by the
    /// torrent.
    pub uploaded: AtomicU64,

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
    SendToSink(Core),

    /// When we download a full piece, we need to send Have's
    /// to peers that dont Have it.
    HavePiece(usize),

    /// Get the pieces of the peer.
    GetPieces(oneshot::Sender<Bitfield>),

    // If the peer supports the local extension id
    // SupportsExt(u8, oneshot::Sender<bool>),
    /// Sometimes a peer either takes too long to answer,
    /// or simply does not answer at all. In both cases
    /// we need to request the block again.
    RequestBlockInfos(Vec<BlockInfo>),

    /// Tell this peer that we choked them
    Choke,

    /// Tell this peer that we unchoked them
    Unchoke,

    /// Tell this peer that we are interested,
    Interested,

    /// Tell this peer that we are not interested,
    NotInterested,

    /// Sends a Cancel message to cancel a block info that we
    /// expect the peer to send us, because we requested it previously.
    CancelBlock(BlockInfo),

    /// Sent when the torrent has downloaded the entire info of the torrent.
    HaveInfo,

    /// Sent when the torrent is paused, it makes the peer pause downloads and
    /// uploads
    Pause,

    /// Sent when the torrent was unpaused.
    Resume,

    /// Sent to make this peer read-only, the peer won't download
    /// anymore, but it will still seed.
    /// This usually happens when the torrent is fully downloaded.
    SeedOnly,

    /// When the program is being gracefuly shutdown, we need to kill the tokio
    /// green thread of the peer.
    GracefullyShutdown,

    /// An error happened and we want to quit and also send this peer's blocks
    /// back to the torrent.
    Quit,
}

/// Determines who initiated the connection.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Direction {
    /// Outbound means we initiated the connection
    Outbound,
    /// Inbound means the peer initiated the connection
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

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const RETRY_INTERVAL: Duration = Duration::from_millis(500);
const MAX_ATTEMPTS: usize = 3;

impl peer::Peer<Idle> {
    pub fn new() -> Self {
        Self { state: Idle {}, state_log: StateLog::default() }
    }

    pub async fn outbound_handshake(
        self,
        socket: TcpStream,
        daemon_ctx: Arc<DaemonCtx>,
        torrent_ctx: Arc<TorrentCtx>,
        metadata_size: u64,
    ) -> Result<peer::Peer<Connected>, Error> {
        let remote = socket.peer_addr()?;
        let local = socket.local_addr()?;
        let mut socket = Framed::new(socket, HandshakeCodec);

        let info_hash = &torrent_ctx.info_hash;

        tracing::debug!("{remote} sending outbound handshake");

        let mut local_handshake =
            Handshake::new(info_hash.clone(), daemon_ctx.local_peer_id.clone());

        if let Some(ext) = local_handshake.ext.as_mut() {
            ext.metadata_size = Some(metadata_size);
        }

        socket.send(local_handshake.clone()).await?;
        socket.flush().await?;

        let mut attempts = 0;

        // --- event loop ---

        let peer_handshake = loop {
            attempts += 1;

            if attempts > 1 {
                sleep(RETRY_INTERVAL).await;
            }

            match timeout(HANDSHAKE_TIMEOUT, socket.next()).await {
                Ok(Some(Ok(handshake))) => break handshake,
                Ok(Some(Err(e))) => {
                    tracing::error!(
                        "handshake decode error: {e}, retrying count: \
                         {attempts}"
                    );

                    let buf = socket.read_buffer();
                    if !buf.is_empty() {
                        warn!(
                            "invalid handshake with buffer len {}",
                            buf.len()
                        );
                    }

                    if attempts < MAX_ATTEMPTS {
                        socket.send(local_handshake.clone()).await?;
                        socket.flush().await?;
                        continue;
                    }

                    return Err(Error::HandshakeInvalid);
                }
                Ok(None) => {
                    // EOF (peer closed connection)
                    tracing::warn!(
                        "peer closed connection during handshake, retrying \
                         count: {attempts}"
                    );
                    let buf = socket.read_buffer();
                    if !buf.is_empty() {
                        warn!(
                            "closed connection with buffer len {}",
                            buf.len()
                        );
                    }
                    if attempts < MAX_ATTEMPTS {
                        socket.send(local_handshake.clone()).await?;
                        socket.flush().await?;
                        continue;
                    }
                    return Err(Error::PeerClosedSocket);
                }
                Err(_) => {
                    // Timeout occurred
                    tracing::warn!(
                        "handshake timeout with after {}ms, retrying count: \
                         {attempts}",
                        HANDSHAKE_TIMEOUT.as_millis()
                    );
                    if attempts < MAX_ATTEMPTS {
                        socket.send(local_handshake.clone()).await?;
                        socket.flush().await?;
                        continue;
                    }
                    return Err(Error::HandshakeTimeout);
                }
            }
        };

        tracing::debug!("{remote} ext {:?}", peer_handshake.ext);

        if !peer_handshake.validate(&local_handshake) {
            warn!("handshake is invalid");
            return Err(Error::HandshakeInvalid);
        }

        let reserved = Reserved::from(peer_handshake.reserved);

        let old_parts = socket.into_parts();
        let mut new_parts = FramedParts::new(old_parts.io, CoreCodec);
        new_parts.read_buf = old_parts.read_buf;
        new_parts.write_buf = old_parts.write_buf;
        let socket = Framed::from_parts(new_parts);

        let (tx, rx) = mpsc::channel::<PeerMsg>(100);

        let ctx = PeerCtx {
            last_download_rate_update: Mutex::new(Instant::now()),
            counter: Counter::default(),
            am_interested: false.into(),
            am_choking: true.into(),
            peer_choking: true.into(),
            peer_interested: false.into(),
            downloaded: 0.into(),
            uploaded: 0.into(),
            direction: Direction::Outbound,
            remote_addr: remote,
            id: peer_handshake.peer_id,
            tx,
            info_hash: peer_handshake.info_hash,
            local_addr: local,
        };

        let (sink, stream) = socket.split();

        let mut peer = peer::Peer {
            state_log: StateLog::default(),
            state: Connected {
                is_paused: false,
                seed_only: false,
                connection: ConnectionState::default(),
                target_request_queue_len: DEFAULT_REQUEST_QUEUE_LEN,
                pieces: Bitfield::new(),
                ctx: Arc::new(ctx),
                ext_states: ExtStates::default(),
                sink,
                stream,
                incoming_requests: Vec::with_capacity(
                    DEFAULT_REQUEST_QUEUE_LEN as usize,
                ),
                outgoing_requests: Vec::with_capacity(
                    DEFAULT_REQUEST_QUEUE_LEN as usize,
                ),
                outgoing_requests_info_pieces: Vec::new(),
                have_info: false,
                in_endgame: false,
                reserved,
                torrent_ctx,
                rx,
            },
        };

        // todo: put this in a function
        if let Some(ext) = peer_handshake.ext {
            let n = ext.reqq.unwrap_or(DEFAULT_REQUEST_QUEUE_LEN);

            peer.state.target_request_queue_len = n;

            // set the peer's extensions
            if ext.m.ut_metadata.is_some() {
                peer.state.ext_states.metadata = Some(MetadataData());
            }

            peer.state.ext_states.extension = Some(ext);
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

        let info = torrent_ctx.info.read().await;

        // if we have the metadata yet
        let metadata_size = match info.metadata_size {
            Some(size) => size,
            None => torrent_ctx.magnet.length().unwrap_or(0),
        };

        let mut our_handshake = Handshake::new(
            peer_handshake.info_hash.clone(),
            daemon_ctx.local_peer_id.clone(),
        );

        if let Some(ext) = &mut our_handshake.ext {
            ext.metadata_size = Some(metadata_size);
        }

        socket.send(our_handshake).await?;
        socket.flush().await?;

        debug!("received peer handshake {peer_handshake:?}",);

        let our_handshake = Handshake::new(
            peer_handshake.info_hash.clone(),
            daemon_ctx.local_peer_id.clone(),
        );

        if !peer_handshake.validate(&our_handshake) {
            warn!("handshake is invalid");
            return Err(Error::HandshakeInvalid);
        }

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

        torrent_ctx.tx.send(TorrentMsg::PeerConnecting(remote)).await?;

        debug!("sending inbound handshake");
        if socket.send(our_handshake).await.is_err() {
            torrent_ctx
                .tx
                .send(TorrentMsg::PeerConnectingError(remote))
                .await?;
        };

        let reserved = Reserved::from(peer_handshake.reserved);

        let old_parts = socket.into_parts();
        let mut new_parts = FramedParts::new(old_parts.io, CoreCodec);
        new_parts.read_buf = old_parts.read_buf;
        new_parts.write_buf = old_parts.write_buf;
        let socket = Framed::from_parts(new_parts);

        let (tx, rx) = mpsc::channel::<PeerMsg>(100);

        let ctx = PeerCtx {
            last_download_rate_update: Mutex::new(Instant::now()),
            counter: Counter::default(),
            am_interested: false.into(),
            am_choking: true.into(),
            peer_choking: true.into(),
            peer_interested: false.into(),
            downloaded: 0.into(),
            uploaded: 0.into(),
            direction: Direction::Inbound,
            remote_addr: remote,
            id: peer_handshake.peer_id,
            tx,
            info_hash: peer_handshake.info_hash,
            local_addr: local,
        };

        let (sink, stream) = socket.split();

        let peer = peer::Peer {
            state_log: StateLog::default(),
            state: Connected {
                is_paused: false,
                seed_only: false,
                connection: ConnectionState::default(),
                target_request_queue_len: DEFAULT_REQUEST_QUEUE_LEN,
                pieces: Bitfield::new(),
                ctx: Arc::new(ctx),
                ext_states: ExtStates::default(),
                sink,
                stream,
                incoming_requests: Vec::with_capacity(50),
                outgoing_requests: Vec::with_capacity(100),
                outgoing_requests_info_pieces: Vec::new(),
                have_info: false,
                in_endgame: false,
                reserved,
                torrent_ctx,
                rx,
            },
        };

        Ok(peer)
    }
}

/// States of peer protocols state, including Core.
/// After a peer handshake, these values may be set if the peer supports them.
#[derive(Default, Clone)]
pub struct ExtStates {
    // BEP02 : The BitTorrent Protocol Specification
    // pub core: CoreState,
    /// BEP10 : Extension Protocol
    pub extension: Option<Extension>,

    /// BEP09 : Extension for Peers to Send Metadata Files
    pub metadata: Option<MetadataData>,

    /// BEP055 : Holepunch extension
    pub holepunch: Option<HolepunchData>,
}

/// Peer is downloading / uploading and working well
pub struct Connected {
    pub stream: SplitStream<Framed<TcpStream, CoreCodec>>,
    pub sink: SplitSink<Framed<TcpStream, CoreCodec>, Core>,
    pub reserved: BitArray<[u8; 8], Msb0>,
    pub torrent_ctx: Arc<TorrentCtx>,
    pub rx: Receiver<PeerMsg>,

    /// a `Bitfield` with pieces that this peer has, and hasn't, containing 0s
    /// and 1s
    pub pieces: Bitfield,

    pub ext_states: ExtStates,

    /// Context of the Peer which is shared for anyone who needs it.
    pub ctx: Arc<PeerCtx>,

    /// Our pending requests that we sent to peer. It represents the blocks
    /// that we are expecting.
    ///
    /// If we receive a block whose request entry is here, that entry is
    /// removed. A request is also removed here when it is timed out.
    pub outgoing_requests: Vec<(BlockInfo, Instant)>,

    // The Instant of each timeout value of [`Self::outgoing_requests`]
    // blocks.
    // pub outgoing_requests_timeout: HashMap<BlockInfo, Instant>,
    /// Outgoing requests of info pieces.
    pub outgoing_requests_info_pieces: Vec<(u64, Instant)>,

    // pub outgoing_requests_info_pieces_times: HashMap<u64, Instant>,
    /// The requests we got from peer.
    ///
    /// The request's entry is removed from here when the block is transmitted
    /// or when the peer cancels it. If a peer sends a request and cancels it
    /// before the disk read is done, the read block is dropped.
    pub incoming_requests: Vec<BlockInfo>,

    /// This is a cache of have_info on Torrent
    /// to avoid using locks or atomics.
    pub have_info: bool,

    /// The current state of the connection.
    pub connection: ConnectionState,

    /// Whether we're in endgame mode.
    pub in_endgame: bool,

    /// The target request queue size is the number of block requests we keep
    /// outstanding
    pub target_request_queue_len: u16,

    /// If the torrent was fully downloaded, all peers will become seed only.
    /// They will only seed but not download anything anymore.
    pub seed_only: bool,

    /// If the client manually paused the local peer, preventing it from
    /// downloading and uploading but keeping connections.
    pub is_paused: bool,
}

/// Tried to do an oubound connection but peer couldn't be reached.
// todo:
// Right now a peer is only converted to this state when we try an outbound
// connection and it doesn't work, if the peer is connected and returns an
// error, we should maybe use another type with more information about why it
// failed, such as a peer being malicious.
#[derive(Clone)]
pub struct PeerError {
    pub addr: SocketAddr,
    pub reconnect_attempts: u32,
}

/// A peer that is being handshaked and soon turned into a connected state.
#[derive(Clone)]
pub struct Connecting {
    pub addr: SocketAddr,
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
impl PeerState for Connecting {}
impl PeerState for Idle {}
