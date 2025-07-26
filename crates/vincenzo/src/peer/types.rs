use std::{
    fmt::Display,
    net::SocketAddr,
    sync::{
        atomic::{AtomicBool, AtomicU64},
        Arc,
    },
};

use bendy::encoding::ToBencode;
use bitvec::{array::BitArray, order::Msb0};
use futures::{
    stream::{SplitSink, SplitStream, StreamExt},
    SinkExt,
};
use hashbrown::{HashMap, HashSet};
use rand::{distr::Alphanumeric, Rng};
use speedy::{Readable, Writable};
use tokio::{
    net::TcpStream,
    sync::{
        mpsc::{self, Receiver},
        oneshot,
    },
    time::Instant,
};
use tokio_util::codec::{Framed, FramedParts};
use tracing::{debug, warn};

use crate::{
    bitfield::{Bitfield, Reserved},
    daemon::{DaemonCtx, DaemonMsg},
    error::Error,
    extensions::{
        core::BlockInfo, Core, CoreCodec, ExtendedMessage, Extension,
        Handshake, HandshakeCodec, MetadataData,
    },
    peer::{self, session::Session},
    torrent::{InfoHash, TorrentCtx, TorrentMsg},
};

#[derive(Clone, PartialEq, Eq, Hash, Default, Readable, Writable)]
pub struct PeerId(pub(crate) [u8; 20]);

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

impl Display for PeerId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", String::from_utf8(self.0.to_vec()).unwrap())
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

    /// Download bytes in the previous 10 seconds, tracked by the torrent.
    pub downloaded: AtomicU64,

    /// Upload bytes in the previous 10 seconds, tracked by the torrent.
    pub uploaded: AtomicU64,

    /// If we're choked, peer doesn't allow us to download pieces from them.
    pub am_choking: AtomicBool,

    /// If we're interested, peer has pieces that we don't have.
    pub am_interested: AtomicBool,

    /// If peer is choked, we don't allow them to download pieces from us.
    pub peer_choking: AtomicBool,

    /// If peer is interested in us, they mean to download pieces that we have.
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

/// Determines who initiated the connection.
#[derive(Clone, Debug, PartialEq)]
pub enum DirectionWithInfoHash {
    /// Outbound means we initiated the connection
    Outbound(InfoHash),
    /// Inbound means the peer initiated the connection
    Inbound,
}

impl From<DirectionWithInfoHash> for Direction {
    fn from(value: DirectionWithInfoHash) -> Self {
        match value {
            DirectionWithInfoHash::Inbound => Direction::Inbound,
            DirectionWithInfoHash::Outbound(_) => Direction::Outbound,
        }
    }
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

impl peer::Peer<Idle> {
    pub fn new() -> Self {
        Self { state: Idle {} }
    }

    /// Do a handshake (and maybe extended handshake) with the peer and convert
    /// it to a connected peer.
    pub async fn handshake(
        self,
        socket: TcpStream,
        daemon_ctx: Arc<DaemonCtx>,
        direction: DirectionWithInfoHash,
    ) -> Result<peer::Peer<Connected>, Error> {
        let remote = socket.peer_addr()?;
        let local = socket.local_addr()?;

        let mut socket = Framed::new(socket, HandshakeCodec);

        if let DirectionWithInfoHash::Outbound(info_hash) = &direction {
            debug!("sending the first handshake to {remote}");

            let our_handshake = Handshake::new(
                info_hash.clone(),
                daemon_ctx.local_peer_id.clone(),
            );

            socket.send(our_handshake).await?;
        }

        // wait and validate their handshake
        let Some(Ok(peer_handshake)) = socket.next().await else {
            warn!("did not send a handshake {remote}");
            return Err(Error::HandshakeInvalid);
        };

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

        // if inbound, he have already received their first handshake,
        // send our second handshake here.
        if direction == DirectionWithInfoHash::Inbound {
            debug!("{remote} sending inbound handshake");
            if socket.send(our_handshake).await.is_err() {
                torrent_ctx
                    .tx
                    .send(TorrentMsg::PeerConnectingError(remote))
                    .await?;
            };
        }

        let reserved = Reserved::from(peer_handshake.reserved);

        let old_parts = socket.into_parts();
        let mut new_parts = FramedParts::new(old_parts.io, CoreCodec);
        new_parts.read_buf = old_parts.read_buf;
        new_parts.write_buf = old_parts.write_buf;
        let mut socket = Framed::from_parts(new_parts);

        // on inbound connection and if the peer supports the extended protocol
        // (BEP010), we send an extended handshake as well, but we
        // receive theirs on the main event loop of Peer<Connected> and
        // not here.
        if reserved[43] && DirectionWithInfoHash::Inbound == direction {
            debug!("{remote} sending extended handshake");

            let magnet = &torrent_ctx.magnet;
            let info = torrent_ctx.info.read().await;

            // if we have the metadata yet
            let metadata_size = match info.pieces() > 0 {
                true => info.metainfo_size().unwrap_or(0),
                false => magnet.length().unwrap_or(0),
            };

            let msg = Extension::supported(Some(metadata_size));
            let msg = ExtendedMessage(0, msg.to_bencode()?);

            if socket.send(msg.into()).await.is_err() {
                torrent_ctx
                    .tx
                    .send(TorrentMsg::PeerConnectingError(remote))
                    .await?;
            }
        }

        let (tx, rx) = mpsc::channel::<PeerMsg>(100);

        let ctx = PeerCtx {
            am_interested: false.into(),
            am_choking: true.into(),
            peer_choking: true.into(),
            peer_interested: false.into(),
            downloaded: 0.into(),
            uploaded: 0.into(),
            direction: direction.into(),
            remote_addr: remote,
            id: peer_handshake.peer_id,
            tx,
            info_hash: peer_handshake.info_hash,
            local_addr: local,
        };

        let (sink, stream) = socket.split();

        let peer = peer::Peer {
            state: Connected {
                pieces: Bitfield::new(),
                ctx: Arc::new(ctx),
                ext_states: ExtStates::default(),
                sink,
                stream,
                incoming_requests: HashSet::default(),
                outgoing_requests: HashSet::default(),
                outgoing_requests_timeout: HashMap::new(),
                session: Session::default(),
                have_info: false,
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

    /// Most of the session's information and state is stored here, i.e. it's
    /// the "context" of the session, with information like: endgame mode, slow
    /// start, download_rate, etc.
    pub session: Session,

    /// Our pending requests that we sent to peer. It represents the blocks
    /// that we are expecting.
    ///
    /// If we receive a block whose request entry is here, that entry is
    /// removed. A request is also removed here when it is timed out.
    pub outgoing_requests: HashSet<BlockInfo>,

    /// The Instant of each timeout value of [`Self::outgoing_requests`]
    /// blocks.
    pub outgoing_requests_timeout: HashMap<BlockInfo, Instant>,

    /// The requests we got from peer.
    ///
    /// The request's entry is removed from here when the block is transmitted
    /// or when the peer cancels it. If a peer sends a request and cancels it
    /// before the disk read is done, the read block is dropped.
    pub incoming_requests: HashSet<BlockInfo>,

    /// This is a cache of have_info on Torrent
    /// to avoid using locks or atomics.
    pub have_info: bool,
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
}

/// A peer that is being handshaked and soon turned into a connected state.
#[derive(Clone)]
pub struct Connecting {
    pub addr: SocketAddr,
}

impl peer::Peer<PeerError> {
    pub fn new(addr: SocketAddr) -> Self {
        peer::Peer { state: PeerError { addr } }
    }
}

impl PeerState for PeerError {}
impl PeerState for Connected {}
impl PeerState for Connecting {}
impl PeerState for Idle {}
