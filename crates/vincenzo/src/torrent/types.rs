use std::{fmt::Display, net::SocketAddr, ops::Deref, sync::Arc};

use speedy::{Readable, Writable};
use tokio::sync::oneshot;

use crate::{
    bitfield::Bitfield,
    extensions::core::BlockInfo,
    peer::{PeerCtx, PeerId},
};

/// Messages used to control the local peer or the state of the torrent.
#[derive(Debug)]
pub enum TorrentMsg {
    /// Message when one of the peers have downloaded
    /// an entire piece. We send Have messages to peers
    /// that don't have it and update the UI with stats.
    DownloadedPiece(usize),
    SetBitfield(usize),
    ReadBitfield(oneshot::Sender<Bitfield>),
    PeerConnected(PeerId, Arc<PeerCtx>),
    DownloadComplete,
    /// When in endgame mode, the first peer that receives this info,
    /// sends this message to send Cancel's to all other peers.
    SendCancelBlock {
        from: PeerId,
        block_info: BlockInfo,
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
    IncrementDownloaded(u64),
    IncrementUploaded(u64),
    /// Toggle pause torrent and send Pause/Resume message to all Peers
    TogglePause,
    /// When we can't do a TCP connection with the ip of the Peer.
    FailedPeer(SocketAddr),
    /// When torrent is being gracefully shutdown
    Quit,
}

#[derive(Clone, PartialEq, Eq, Hash, Default, Readable, Writable)]
pub struct InfoHash([u8; 20]);

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
        f.write_str(&s)
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
