use std::{collections::BTreeMap, io};

use thiserror::Error;
use tokio::sync::{mpsc, oneshot};

use crate::{
    daemon::DaemonMsg,
    disk::DiskMsg,
    extensions::Block,
    peer::{PeerId, PeerMsg},
    torrent::TorrentMsg,
    tracker::TrackerMsg,
};

impl From<bendy::decoding::Error> for Error {
    fn from(_value: bendy::decoding::Error) -> Self {
        Self::BencodeError
    }
}

impl From<bendy::encoding::Error> for Error {
    fn from(_value: bendy::encoding::Error) -> Self {
        Self::BencodeError
    }
}

impl From<mpsc::error::SendError<DaemonMsg>> for Error {
    fn from(value: mpsc::error::SendError<DaemonMsg>) -> Self {
        Self::SendDaemonError(value.to_string())
    }
}

impl From<Option<BTreeMap<u32, Block>>> for Error {
    fn from(_value: Option<BTreeMap<u32, Block>>) -> Self {
        Self::TorrentDoesNotExist
    }
}

#[derive(Error, Debug)]
pub enum Error {
    #[error("Failed to send a connect request to the tracker")]
    ConnectSendFailed,

    #[error("")]
    FrontendError,

    #[error("Failed to send a connect request to the tracker")]
    MagnetError(#[from] magnet_url::MagnetError),

    #[error("String is not UTF-8")]
    Utf8Error(#[from] std::string::FromUtf8Error),

    #[error("The given peer id was not found: {0}")]
    PeerNotFound(PeerId),

    #[error("Tried to unchoke a peer but the maximum amount is already full")]
    MaximumUnchokedPeers,

    #[error("Failed to encode")]
    BincodeEncodeError(#[from] bincode::error::EncodeError),

    #[error("Failed to dencode")]
    BincodeDecodeError(#[from] bincode::error::DecodeError),

    #[error("Configuration error: {0}")]
    ConfigError(String),

    #[error("Failed to decode or encode the bencode buffer")]
    BencodeError,

    #[error("IO error")]
    IO(#[from] io::Error),

    #[error("Peer resolved to no unusable addresses")]
    PeerSocketAddr,

    #[error("Tracker resolved to no unusable addresses")]
    TrackerNoHosts,

    #[error("Peer resolved to no unusable addresses")]
    TrackerSocketAddr,

    #[error("The response received from the connect handshake was wrong")]
    TrackerResponse,

    #[error(
        "Tracker event only goes from 0..=2 and a different value was used"
    )]
    TrackerEvent,

    #[error("Tried to call announce without calling connect first")]
    TrackerResponseLength,

    #[error(
        "The response length received from the tracker was less then 20 \
         bytes, when it should be larger"
    )]
    TrackerNoConnectionId,

    #[error("The peer list returned by the announce request is not valid")]
    TrackerCompactPeerList,

    #[error("Could not connect to the UDP socket of the tracker")]
    TrackerSocketConnect,

    #[error("Error when serializing/deserializing")]
    SpeedyError(#[from] speedy::Error),

    #[error("Error while trying to load configuration: `{0}")]
    FromConfigError(#[from] config::ConfigError),

    #[error("Error when reading magnet link")]
    MagnetLinkInvalid,

    #[error("The response received from the peer is wrong")]
    MessageResponse,

    #[error("The request took to long to arrive")]
    RequestTimeout,

    #[error("The message took to long to arrive")]
    MessageTimeout,

    #[error("The handshake received is not valid")]
    HandshakeInvalid,

    #[error("The peer took to long to send the handshake")]
    HandshakeTimeout,

    #[error("The peer didn't send a handshake as the first message")]
    NoHandshake,

    #[error(
        "Could not open the file `{0}`. Please make sure the program has \
         permission to access it"
    )]
    FileOpenError(String),

    #[error(
        "Could not open the folder `{0}`. Please make sure the program has \
         permission to open it and that the folder exist"
    )]
    FolderOpenError(String),

    #[error(
        "The `{0}` folder was not found, please edit the config file manually \
         at `{1}"
    )]
    FolderNotFound(String, String),

    #[error("This torrent is already downloaded fully")]
    TorrentComplete,

    #[error("Could not find torrent for the given info_hash")]
    TorrentDoesNotExist,

    #[error("The piece downloaded does not have a valid hash")]
    PieceInvalid,

    #[error("The peer ID does not exist on this torrent")]
    PeerIdInvalid,

    #[error("The peer closed the socket")]
    PeerClosedSocket,

    #[error("Disk does not have the provided info_hash")]
    InfoHashInvalid,

    #[error("The peer took to long to respond")]
    Timeout,

    #[error(
        "Your magnet does not have a tracker. Currently, this client does not \
         support DHT, you need to use a magnet that has a tracker."
    )]
    MagnetNoTracker,

    #[error(
        "Your magnet does not have an info_hash, are you sure you copied the \
         entire magnet link?"
    )]
    MagnetNoInfoHash,

    #[error("Could not send message to Disk")]
    SendErrorDisk(#[from] mpsc::error::SendError<DiskMsg>),

    #[error("Could not send message to Daemon: {0}")]
    SendDaemonError(String),

    #[error("Could not receive message from oneshot")]
    ReceiveErrorOneshot(#[from] oneshot::error::RecvError),

    #[error("Could not send message to Peer")]
    SendErrorPeer(#[from] mpsc::error::SendError<PeerMsg>),

    #[error("Could not send message to Tracker")]
    SendErrorTracker(#[from] mpsc::error::SendError<TrackerMsg>),

    #[error("Could not send message to UI")]
    SendErrorTorrent(#[from] mpsc::error::SendError<TorrentMsg>),

    #[error("The given PATH is invalid")]
    PathInvalid,

    #[error("Could not send message to TCP socket")]
    SendErrorTcp,

    #[error(
        "Tried to load $HOME but could not find it. Please make sure you have \
         a $HOME env and that this program has the permission to create dirs."
    )]
    HomeInvalid,

    #[error(
        "Error while trying to read the configuration file, please make sure \
         it has the correct format"
    )]
    ConfigDeserializeError,

    #[error("You cannot add a duplicate torrent, only 1 is allowed")]
    NoDuplicateTorrent,

    #[error("No peers in the torrent")]
    NoPeers,
}
