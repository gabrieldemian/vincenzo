use std::{
    collections::BTreeMap,
    fmt::Display,
    net::{IpAddr, SocketAddr},
    ops::Deref,
    sync::Arc,
};

use bincode::{Decode, Encode};
use rand::Rng;
use speedy::{Readable, Writable};
use tokio::sync::oneshot;

use crate::{
    bitfield::Bitfield,
    extensions::core::BlockInfo,
    peer::{PeerCtx, PeerId},
};

/// Broadcasted messages for all peers in a torrent.
#[derive(Debug, Clone)]
pub enum TorrentBrMsg {
    /// Start endgame mode.
    Endgame,

    /// Send request blocks to all peers.
    Request { blocks: BTreeMap<usize, Vec<BlockInfo>> },

    /// When in endgame mode, the first peer that receives this info,
    /// sends this message to send Cancel's to all other peers.
    Cancel { from: PeerId, block_info: BlockInfo },
}

/// Messages used to control the local peer or the state of the torrent.
#[derive(Debug)]
pub enum TorrentMsg {
    /// Message when one of the peers have downloaded
    /// an entire piece. We send Have messages to peers
    /// that don't have it and update the UI with stats.
    DownloadedPiece(usize),

    /// Received when a peer sent a metadata size on extended handshake.
    MetadataSize(u64),

    /// When a peer downloads an info piece,
    /// we need to mutate `info_dict` and maybe
    /// generate the entire info.
    /// total, metadata.index, bytes
    DownloadedInfoPiece(u64, u64, Vec<u8>),

    ReadBitfield(oneshot::Sender<Bitfield>),

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

    /// Error when handshaking a peer, even though the TCP connection was
    /// established.
    PeerConnectingError(SocketAddr),

    /// Start endgame mode, sent by the disk.
    Endgame,

    /// When a peer request a piece of the info
    /// index, recipient
    RequestInfoPiece(u64, oneshot::Sender<Option<Vec<u8>>>),

    IncrementDownloaded(u64),
    IncrementUploaded(u64),

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
    pub download_rate: u64,
    pub uploaded: u64,
    pub upload_rate: u64,
    pub size: u64,
    pub info_hash: InfoHash,
    pub have_info: bool,
    pub bitfield: Vec<u8>,
    pub connected_peers: u8,
    pub downloading_from: u8,
    pub idle_peers: u8,
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
