use crate::{
    bitfield::Bitfield,
    extensions::core::BlockInfo,
    magnet::Magnet,
    metainfo::{Info, InfoHash, MetaInfo},
    peer::{self, Peer, PeerCtx, PeerId},
    torrent::{self, Torrent},
    tracker::TrackerMsg,
};
use rkyv::{Archive, Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fmt::Display,
    net::{IpAddr, SocketAddr},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::{
    sync::{broadcast, oneshot},
    time::Interval,
};

/// Broadcasted messages for all peers in a torrent.
#[derive(Debug, Clone)]
pub enum PeerBrMsg {
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
    Promote(Box<Info>),
    Endgame,
    SendToAllPeers(PeerId, Vec<BlockInfo>, Vec<BlockInfo>),

    /// When a peer wants to request blocks.
    Request {
        peer_ctx: Arc<PeerCtx>,
        qnt: usize,
        recipient: oneshot::Sender<Result<Vec<BlockInfo>, crate::Error>>,
    },

    /// Make the torrent run the unchoke algorithm.
    UnchokeAlgorithm,

    SetTorrentError(TorrentStatusErrorCode),

    /// Make the torrent run the optimistic unchoke algorithm.
    OptUnchokeAlgorithm,

    GetUnchokedPeers(oneshot::Sender<Vec<Arc<PeerCtx>>>),

    /// Get the peer's context.
    GetPeer(PeerId, oneshot::Sender<Option<Arc<PeerCtx>>>),

    /// Message when one of the peers have downloaded
    /// an entire piece. We send Have messages to peers
    /// that don't have it and update the UI with stats.
    DownloadedPiece(usize),

    /// Clone block infos to the peer.
    ///
    /// In the case where peers already exchanged block infos in the endgame
    /// mode, and the client connects with another peer.
    WantBlocks(usize, Arc<PeerCtx>),

    /// Sent by the tracker on periodic announces to add more peers to be
    /// connected.
    AddIdlePeers(HashSet<SocketAddr>),

    GetTorrentStatus(oneshot::Sender<TorrentStatus>),

    HaveInfo(oneshot::Sender<bool>),

    /// When a peer downloads an info piece,
    /// we need to mutate `info_dict` and maybe
    /// generate the entire info.
    /// total, metadata.index, bytes
    DownloadedInfoPiece(usize, u64, Vec<u8>),

    ReadBitfield(oneshot::Sender<Bitfield>),

    PeerConnected(Arc<PeerCtx>),

    /// When we can't do a TCP connection with the ip of the Peer.
    PeerError(SocketAddr),

    SetPeerBitfield(PeerId, Bitfield),

    /// If the remote peer has a piece in which the local hasn't
    /// Returns Some with the absent piece or None if local has it.
    PeerHasPieceNotInLocal(PeerId, oneshot::Sender<Option<usize>>),

    /// Set a piece of the peer's bitfield to true
    PeerHave(PeerId, usize),

    /// When a peer request a piece of the info
    /// index, recipient
    RequestInfoPiece(u64, oneshot::Sender<Option<Vec<u8>>>),

    /// Toggle pause torrent and send Pause/Resume message to all Peers
    TogglePause,

    /// When torrent is being gracefully shutdown
    Quit,

    Cancel(PeerId, BlockInfo),
}

#[derive(
    Debug, Default, Clone, Copy, PartialEq, Archive, Serialize, Deserialize,
)]
#[rkyv(compare(PartialEq), derive(Debug))]
pub enum TorrentStatus {
    #[default]
    ConnectingTrackers,
    DownloadingMetainfo,
    Downloading,
    Seeding,
    Paused,
    Error(TorrentStatusErrorCode),
}

#[derive(
    Debug, Default, Clone, Copy, PartialEq, Archive, Serialize, Deserialize,
)]
#[rkyv(compare(PartialEq), derive(Debug))]
pub enum TorrentStatusErrorCode {
    /// Errors not handled by the code.
    #[default]
    Unknown,

    /// Files missing from a completed torrent, likely due to the user fault.
    FilesMissing,
}

impl Display for TorrentStatusErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            TorrentStatusErrorCode::Unknown => "Unknown error".to_string(),
            TorrentStatusErrorCode::FilesMissing => {
                "Missing files on disk".to_string()
            }
        };
        write!(f, "{}", s)
    }
}

/// State of a [`Torrent`], used by the UI to present data.
#[derive(Debug, Clone, Default, PartialEq, Archive, Deserialize, Serialize)]
#[rkyv(compare(PartialEq), derive(Debug))]
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

impl From<&Torrent<torrent::Idle, FromMagnet>> for TorrentState {
    fn from(value: &Torrent<torrent::Idle, FromMagnet>) -> Self {
        Self {
            name: value.name.clone(),
            status: TorrentStatus::ConnectingTrackers,
            bitfield: value.bitfield.clone().into(),
            info_hash: value.source.magnet.parse_xt_infohash(),
            ..Default::default()
        }
    }
}

impl From<&Torrent<torrent::Idle, FromMetaInfo>> for TorrentState {
    fn from(value: &Torrent<torrent::Idle, FromMetaInfo>) -> Self {
        Self {
            name: value.name.clone(),
            status: TorrentStatus::ConnectingTrackers,
            bitfield: value.bitfield.clone().into(),
            info_hash: value.source.meta_info.info.info_hash.clone(),
            ..Default::default()
        }
    }
}

impl<M: TorrentSource> From<&Torrent<torrent::Connected, M>> for TorrentState {
    fn from(value: &Torrent<torrent::Connected, M>) -> Self {
        let downloading_from = match value.status {
            TorrentStatus::Seeding => {
                value.state.connected_peers.iter().fold(0, |acc, v| {
                    acc + if v.peer_interested.load(Ordering::Relaxed) {
                        1
                    } else {
                        0
                    }
                })
            }
            _ => value.state.connected_peers.iter().fold(0, |acc, v| {
                acc + if !v.peer_choking.load(Ordering::Relaxed)
                    && v.am_interested.load(Ordering::Relaxed)
                {
                    1
                } else {
                    0
                }
            }),
        };

        Self {
            name: value.name.clone(),
            stats: value.state.stats.clone(),
            info_hash: InfoHash::default(),
            size: value.ctx.size.load(Ordering::Relaxed),
            status: value.status,
            downloaded: value.ctx.counter.total_download(),
            uploaded: value.ctx.counter.total_upload(),
            bitfield: value.bitfield.clone().into_vec(),
            idle_peers: value.state.idle_peers.len() as u8,
            connected_peers: value.state.connected_peers.len() as u8,
            download_rate: value.ctx.counter.download_rate_f64(),
            upload_rate: value.ctx.counter.upload_rate_f64(),
            downloading_from,
        }
    }
}

/// Status of the current Torrent, updated at every announce request.
#[derive(Clone, Debug, PartialEq, Default, Archive, Serialize, Deserialize)]
#[rkyv(compare(PartialEq), derive(Debug))]
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
            Error(_) => "Error",
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
            Error(_) => "Error".to_owned(),
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
            _ => Error(TorrentStatusErrorCode::Unknown),
        }
    }
}

pub(crate) trait State {}

/// If the torrent came from a magnet or metainfo.
pub(crate) trait TorrentSource {
    fn organize_trackers(&self) -> HashMap<&str, Vec<String>>;
}

pub(crate) struct FromMagnet {
    pub magnet: Magnet,
    // pub info: Option<Arc<Info>>,
}

pub(crate) struct FromMetaInfo {
    pub meta_info: MetaInfo,
}

impl TorrentSource for FromMagnet {
    fn organize_trackers(&self) -> HashMap<&str, Vec<String>> {
        self.magnet.organize_trackers()
    }
}
impl TorrentSource for FromMetaInfo {
    fn organize_trackers(&self) -> HashMap<&str, Vec<String>> {
        self.meta_info.organize_trackers()
    }
}

// States of the torrent, idle is when the tracker is not connected and the
// torrent is not being downloaded
pub(crate) struct Idle {}

pub(crate) struct Connected {
    /// Stats of the current Torrent, returned from tracker on announce
    /// requests.
    pub stats: Stats,

    /// Lock the torrent so only one peer can do something at a time.
    pub stealing: Arc<AtomicBool>,

    /// If using a Magnet link, the info will be downloaded in pieces
    /// and those pieces may come in different order,
    /// After it is complete, it will be encoded into [`Info`]
    pub info_pieces: BTreeMap<u64, Vec<u8>>,

    /// Idle peers returned from an announce request to the tracker.
    /// Will be removed from this vec as we connect with them, and added as we
    /// request more peers to the tracker.
    pub idle_peers: HashSet<SocketAddr>,
    pub error_peers: Vec<Peer<peer::PeerError>>,
    pub connected_peers: Vec<Arc<PeerCtx>>,

    /// Maximum of 3 unchoked peers as per the protocol + the optimistically
    /// unchoked peer = 4. These come from `connected_peers`.
    pub unchoked_peers: Vec<Arc<PeerCtx>>,

    /// Only one optimistically unchoked peer for 30 seconds.
    pub opt_unchoked_peer: Option<Arc<PeerCtx>>,

    /// Bitfield of each peer
    pub peer_pieces: HashMap<PeerId, Box<Bitfield>>,

    /// Pieces that the remote peer has and the client doesn't.
    pub peer_pieces_diff: HashMap<PeerId, Bitfield>,

    /// Requested peer pieces
    pub peer_pieces_req: Bitfield,

    pub tracker_tx: broadcast::Sender<TrackerMsg>,
    pub reconnect_interval: Interval,
    pub heartbeat_interval: Interval,
    pub optimistic_unchoke_interval: Interval,
    pub unchoke_interval: Interval,
}

impl State for Idle {}
impl State for Connected {}
