use std::{fmt::Display, net::SocketAddr, sync::Arc};

use bitvec::{array::BitArray, order::Msb0};
use futures::{
    stream::{SplitSink, SplitStream, StreamExt},
    SinkExt,
};
use hashbrown::{HashMap, HashSet};
use speedy::{Readable, Writable};
use tokio::{
    net::TcpStream,
    sync::{
        mpsc::{self, Receiver},
        RwLock,
    },
    time::Instant,
};
use tokio_util::codec::{Framed, FramedParts};
use tracing::{debug, warn};

use crate::{
    bitfield::{Bitfield, Reserved},
    error::Error,
    extensions::{
        core::BlockInfo, Core, CoreCodec, Extended, ExtendedMessage, Extension,
        Handshake, HandshakeCodec, TryIntoExtendedMessage,
    },
    peer::{self, session::Session, ExtStates, PeerCtx},
    torrent::{InfoHash, TorrentCtx, TorrentMsg},
};

#[derive(Clone, PartialEq, Eq, Hash, Default, Readable, Writable)]
pub struct PeerId([u8; 20]);

impl Display for PeerId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", hex::encode(self.0))
    }
}

impl TryInto<PeerId> for String {
    type Error = String;
    fn try_into(self) -> Result<PeerId, Self::Error> {
        let buff = hex::decode(self).map_err(|e| e.to_string())?;
        let hash = PeerId::try_from(buff)?;
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

/// Messages used to control the peer state or to make the peer forward a
/// message.
#[derive(Debug)]
pub enum PeerMsg {
    SendToSink(Core),
    /// When we download a full piece, we need to send Have's
    /// to peers that dont Have it.
    HavePiece(usize),
    /// Sometimes a peer either takes too long to answer,
    /// or simply does not answer at all. In both cases
    /// we need to request the block again.
    RequestBlockInfos(Vec<BlockInfo>),
    /// Tell this peer that we are not interested,
    /// update the local state and send a message to the peer
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
    /// An error happened and we want to quit and also send this peer's blocks back to the torrent.
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
pub struct Idle {
    pub direction: Direction,
    pub info_hash: InfoHash,
    pub local_peer_id: PeerId,
}

impl peer::Peer<Idle> {
    pub fn new(
        direction: Direction,
        info_hash: InfoHash,
        local_peer_id: PeerId,
    ) -> Self {
        Self { state: Idle { direction, info_hash, local_peer_id } }
    }

    /// Do a handshake (and maybe extended handshake) with the peer and convert
    /// it to a connected peer.
    pub async fn handshake(
        mut self,
        socket: TcpStream,
        torrent_ctx: Arc<TorrentCtx>,
    ) -> Result<peer::Peer<Connected>, Error> {
        let remote = socket.peer_addr()?;

        torrent_ctx.tx.send(TorrentMsg::PeerConnecting(remote.clone())).await?;

        let local = socket.local_addr()?;

        let mut socket = Framed::new(socket, HandshakeCodec);
        let info_hash = self.state.info_hash;

        let our_handshake =
            Handshake::new(info_hash.clone(), self.state.local_peer_id);

        let peer_handshake: Handshake;

        // if we are connecting, send the first handshake
        if self.state.direction == Direction::Outbound {
            debug!("sending the first handshake to {remote}");
            socket.send(our_handshake.clone()).await?;
        }

        // wait for, and validate, their handshake
        if let Some(Ok(their_handshake)) = socket.next().await {
            debug!("received their handshake {remote}");

            if !their_handshake.validate(&our_handshake) {
                return Err(Error::HandshakeInvalid);
            }

            peer_handshake = their_handshake;
        } else {
            warn!("did not send a handshake {remote}");
            return Err(Error::HandshakeInvalid);
        }

        let reserved = Reserved::from(peer_handshake.reserved);

        if self.state.direction == Direction::Inbound {
            debug!("sending the second handshake to {remote}");
            socket.send(our_handshake).await?;
        }

        let old_parts = socket.into_parts();
        let mut new_parts = FramedParts::new(old_parts.io, CoreCodec);
        new_parts.read_buf = old_parts.read_buf;
        new_parts.write_buf = old_parts.write_buf;
        let mut socket = Framed::from_parts(new_parts);

        // on inbound connection and if the peer supports the extended protocol
        // (BEP010), we send an extended handshake as well, but we
        // receive theirs on the main event loop of Peer<Connected> and
        // not here.
        if reserved[43] && self.state.direction == Direction::Inbound {
            debug!("sending extended handshake to {remote}");

            let metadata_size = torrent_ctx.info.read().await.metainfo_size();

            let msg: ExtendedMessage =
                Extension::supported(Some(metadata_size as u32)).try_into()?;

            socket.send(msg.into()).await?;
        }

        let (tx, rx) = mpsc::channel::<PeerMsg>(100);

        let ctx = PeerCtx {
            direction: self.state.direction,
            remote_addr: remote,
            pieces: RwLock::new(Bitfield::new()),
            id: peer_handshake.peer_id.into(),
            tx,
            info_hash,
            local_addr: local,
        };

        let (sink, stream) = socket.split();

        let peer = peer::Peer {
            state: Connected {
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

/// Peer is downloading / uploading and working well
pub struct Connected {
    pub stream: SplitStream<Framed<TcpStream, CoreCodec>>,
    pub sink: SplitSink<Framed<TcpStream, CoreCodec>, Core>,
    pub reserved: BitArray<[u8; 8], Msb0>,
    pub torrent_ctx: Arc<TorrentCtx>,
    pub rx: Receiver<PeerMsg>,

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
