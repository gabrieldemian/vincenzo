use std::{
    collections::BTreeMap,
    fmt::Display,
    net::{IpAddr, SocketAddr},
    ops::Deref,
    sync::{Arc, atomic::Ordering},
};

use bincode::{Decode, Encode};
use hashbrown::{HashMap, HashSet};
use rand::Rng;
use speedy::{Readable, Writable};
use tokio::{
    sync::{broadcast, oneshot},
    time::Interval,
};

use crate::{
    bitfield::Bitfield,
    counter::Counter,
    extensions::core::BlockInfo,
    magnet::Magnet,
    metainfo::{Info, MetaInfo},
    peer::{self, Peer, PeerCtx, PeerId},
    torrent::{self, Torrent},
    tracker::TrackerMsg,
};

/// Broadcasted messages for all peers in a torrent.
#[derive(Debug, Clone)]
pub enum PeerBrMsg {
    /// Start endgame mode with block infos sent by the disk.
    Endgame(BTreeMap<usize, Vec<BlockInfo>>),

    /// Send request blocks to all peers.
    Request(BTreeMap<usize, Vec<BlockInfo>>),

    /// When a new peer is connected, other peers might want to send messages
    /// to him to syncronize state.
    NewPeer(Arc<PeerCtx>),

    /// When in endgame mode, the first peer that receives this info,
    /// sends this message to send Cancel's to all other peers.
    Cancel(BlockInfo),

    /// The download finished
    Seedonly,
    HavePiece(usize),
    Pause,
    Resume,
    Quit,
    HaveInfo,
}

/// Messages used to control the local peer or the state of the torrent.
#[derive(Debug)]
pub enum TorrentMsg {
    /// Message when one of the peers have downloaded
    /// an entire piece. We send Have messages to peers
    /// that don't have it and update the UI with stats.
    DownloadedPiece(usize),

    /// Sent by the tracker on periodic announces to add more peers to be
    /// connected.
    AddIdlePeers(HashSet<SocketAddr>),

    /// downloaded, uploaded, left
    GetAnnounceData(oneshot::Sender<(u64, u64, u64)>),

    /// Received when a peer sent a metadata size on extended handshake.
    MetadataSize(usize),

    GetTorrentStatus(oneshot::Sender<TorrentStatus>),

    HaveInfo(oneshot::Sender<bool>),

    GetMetadataSize(oneshot::Sender<Option<usize>>),

    /// When a peer downloads an info piece,
    /// we need to mutate `info_dict` and maybe
    /// generate the entire info.
    /// total, metadata.index, bytes
    DownloadedInfoPiece(usize, u64, Vec<u8>),

    ReadBitfield(oneshot::Sender<Bitfield>),

    GetAnnounceList(oneshot::Sender<Vec<String>>),

    /// Sent when the peer is acting as a relay for the holepunch protocol.
    ReadPeerByIp(IpAddr, u16, oneshot::Sender<Option<Arc<PeerCtx>>>),

    PeerConnecting(SocketAddr),

    GetConnectedPeers(oneshot::Sender<Vec<Arc<PeerCtx>>>),

    PeerConnected(Arc<PeerCtx>),

    /// When we can't do a TCP connection with the ip of the Peer.
    PeerError(SocketAddr),

    /// Get the missing pieces of local. Where 1 = missing, 0 = not missing
    GetMissingPieces(PeerId, oneshot::Sender<Bitfield>),

    SetPeerBitfield(PeerId, Bitfield),

    /// If the remote peer has a piece in which the local hasn't
    /// Returns Some with the absent piece or None if local has it.
    PeerHasPieceNotInLocal(PeerId, oneshot::Sender<Option<usize>>),

    /// Clone the peer's bitfield.
    GetPeerBitfield(PeerId, oneshot::Sender<Option<Bitfield>>),

    /// Set a piece of the peer's bitfield to true
    PeerHave(PeerId, usize),

    /// Start endgame mode, sent by the disk.
    Endgame(BTreeMap<usize, Vec<BlockInfo>>),

    /// When a peer request a piece of the info
    /// index, recipient
    RequestInfoPiece(u64, oneshot::Sender<Option<Vec<u8>>>),

    /// Toggle pause torrent and send Pause/Resume message to all Peers
    TogglePause,

    /// When torrent is being gracefully shutdown
    Quit,
}

#[derive(
    Clone, PartialEq, Eq, Hash, Default, Readable, Writable, Encode, Decode,
)]
pub struct InfoHash(pub [u8; 20]);

impl InfoHash {
    pub fn random() -> Self {
        InfoHash(rand::rng().random())
    }
}

impl Display for InfoHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", hex::encode(self.0))
    }
}

impl Deref for InfoHash {
    type Target = [u8; 20];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::fmt::Debug for InfoHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = self.to_string();
        let s = s.get(..10).unwrap();
        f.write_str(s)
    }
}

impl From<InfoHash> for [u8; 20] {
    fn from(value: InfoHash) -> Self {
        value.0
    }
}

impl From<InfoHash> for String {
    fn from(value: InfoHash) -> Self {
        value.to_string()
    }
}

impl TryInto<InfoHash> for String {
    type Error = String;
    fn try_into(self) -> Result<InfoHash, Self::Error> {
        let buff = hex::decode(self).map_err(|e| e.to_string())?;
        let hash = InfoHash::try_from(buff)?;
        Ok(hash)
    }
}

impl From<[u8; 20]> for InfoHash {
    fn from(value: [u8; 20]) -> Self {
        Self(value)
    }
}

impl TryFrom<Vec<u8>> for InfoHash {
    type Error = &'static str;
    fn try_from(value: Vec<u8>) -> Result<Self, Self::Error> {
        if value.len() != 20 {
            return Err("The infohash must have exactly 20 bytes");
        }
        let mut buff = [0u8; 20];
        buff[..20].copy_from_slice(&value[..20]);
        Ok(InfoHash(buff))
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Encode, Decode)]
pub enum TorrentStatus {
    #[default]
    ConnectingTrackers,
    DownloadingMetainfo,
    Downloading,
    Seeding,
    Paused,
    Error,
}

/// State of a [`Torrent`], used by the UI to present data.
#[derive(Debug, Clone, Default, PartialEq, Encode, Decode)]
pub struct TorrentState {
    pub name: String,
    pub stats: Stats,
    pub status: TorrentStatus,
    pub downloaded: u64,
    pub download_rate: f64,
    pub uploaded: u64,
    pub upload_rate: f64,
    pub size: u64,
    pub info_hash: InfoHash,
    pub bitfield: Vec<u8>,
    pub connected_peers: u8,
    pub downloading_from: u8,
    pub idle_peers: u8,
}

impl<M: TorrentSource> From<&Torrent<torrent::Idle, M>> for TorrentState {
    fn from(value: &Torrent<torrent::Idle, M>) -> Self {
        Self {
            name: value.name.clone(),
            status: TorrentStatus::ConnectingTrackers,
            bitfield: value.bitfield.clone().into(),
            info_hash: value.source.info_hash(),
            ..Default::default()
        }
    }
}

impl<M: TorrentSource> From<&Torrent<torrent::Connected, M>> for TorrentState {
    fn from(value: &Torrent<torrent::Connected, M>) -> Self {
        let downloading_from =
            value.state.connected_peers.iter().fold(0, |acc, v| {
                acc + if !v.peer_choking.load(Ordering::Relaxed)
                    && v.am_interested.load(Ordering::Relaxed)
                {
                    1
                } else {
                    0
                }
            });

        Self {
            name: value.name.clone(),
            stats: value.state.stats.clone(),
            info_hash: value.source.info_hash(),
            size: value.state.size,
            status: value.status,
            downloaded: value.state.counter.total_download(),
            uploaded: value.state.counter.total_upload(),
            bitfield: value.bitfield.clone().into_vec(),
            idle_peers: value.state.idle_peers.len() as u8,
            connected_peers: value.state.connected_peers.len() as u8,
            download_rate: value.state.counter.download_rate_f64(),
            upload_rate: value.state.counter.upload_rate_f64(),
            downloading_from,
        }
    }
}

/// Status of the current Torrent, updated at every announce request.
#[derive(
    Clone, Debug, PartialEq, Default, Readable, Writable, Encode, Decode,
)]
pub struct Stats {
    pub interval: u32,
    pub leechers: u32,
    pub seeders: u32,
}

impl From<TorrentStatus> for &str {
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

pub trait State {}

/// If the torrent came from a magnet or metainfo.
pub trait TorrentSource {
    fn organize_trackers(&self) -> HashMap<&str, Vec<String>>;
    fn info_hash(&self) -> InfoHash;
    fn size(&self) -> u64;
}

pub struct FromMagnet {
    pub magnet: Magnet,
    pub info: Option<Info>,
}

pub struct FromMetaInfo {
    pub meta_info: MetaInfo,
}

impl TorrentSource for FromMagnet {
    fn organize_trackers(&self) -> HashMap<&str, Vec<String>> {
        self.magnet.organize_trackers()
    }
    fn info_hash(&self) -> InfoHash {
        self.magnet.parse_xt_infohash()
    }
    fn size(&self) -> u64 {
        self.magnet.length().unwrap_or(0)
    }
}
impl TorrentSource for FromMetaInfo {
    fn organize_trackers(&self) -> HashMap<&str, Vec<String>> {
        self.meta_info.organize_trackers()
    }
    fn info_hash(&self) -> InfoHash {
        self.meta_info.info.info_hash.clone()
    }
    fn size(&self) -> u64 {
        self.meta_info.info.get_torrent_size() as u64
    }
}

// States of the torrent, idle is when the tracker is not connected and the
// torrent is not being downloaded
pub struct Idle {
    pub metadata_size: Option<usize>,
}

pub struct Connected {
    /// Stats of the current Torrent, returned from tracker on announce
    /// requests.
    pub stats: Stats,

    pub counter: Counter,

    /// If using a Magnet link, the info will be downloaded in pieces
    /// and those pieces may come in different order,
    /// After it is complete, it will be encoded into [`Info`]
    pub info_pieces: BTreeMap<u64, Vec<u8>>,

    /// The size of the entire torrent in disk, in bytes.
    pub size: u64,

    /// Idle peers returned from an announce request to the tracker.
    /// Will be removed from this vec as we connect with them, and added as we
    /// request more peers to the tracker.
    pub idle_peers: HashSet<SocketAddr>,

    /// Idle peers being handshaked and soon moved to `connected_peer`.
    pub connecting_peers: Vec<SocketAddr>,

    /// Connected peers, removed from `peers`.
    pub connected_peers: Vec<Arc<PeerCtx>>,

    /// Maximum of 3 unchoked peers as per the protocol + the optimistically
    /// unchoked peer = 4. These come from `connected_peers`.
    pub unchoked_peers: Vec<Arc<PeerCtx>>,

    /// Only one optimistically unchoked peer for 30 seconds.
    pub opt_unchoked_peer: Option<Arc<PeerCtx>>,

    pub error_peers: Vec<Peer<peer::PeerError>>,

    /// Size of the `info` bencoded string.
    pub metadata_size: Option<usize>,

    /// Pieces that all peers have.
    pub peer_pieces: HashMap<PeerId, Bitfield>,

    pub tracker_tx: broadcast::Sender<TrackerMsg>,

    pub(crate) reconnect_interval: Interval,
    pub(crate) heartbeat_interval: Interval,
    pub(crate) log_rates_interval: Interval,
    pub(crate) optimistic_unchoke_interval: Interval,
    pub(crate) unchoke_interval: Interval,
}

impl State for Idle {}
impl State for Connected {}
