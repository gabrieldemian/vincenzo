use crate::{
    bitfield::Bitfield, daemon::DaemonMsg, disk::DiskMsg, peer::PeerMsg,
    torrent::TorrentMsg,
};
use std::{io, num::ParseIntError, path::PathBuf, str::ParseBoolError};
use thiserror::Error;
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinError,
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

impl From<mpsc::error::SendError<DiskMsg>> for Error {
    fn from(_value: mpsc::error::SendError<DiskMsg>) -> Self {
        Self::SendErrorDisk
    }
}

impl From<std::convert::Infallible> for Error {
    fn from(value: std::convert::Infallible) -> Self {
        match value {}
    }
}

impl From<Error> for ! {
    fn from(_: Error) -> Self {
        panic!("wtf")
    }
}

#[derive(Error, Debug)]
pub enum Error {
    #[error(
        "Received wrong extension ID: `{received}` different from the local: \
         `{received}`"
    )]
    WrongExtensionId { local: u8, received: u8 },

    #[error("Missing files on disk")]
    TorrentFilesMissing(Bitfield),

    #[error("File not found: {0}")]
    FileNotFound(PathBuf),

    #[error("")]
    FrontendError,

    #[error("Join error: {0}")]
    JoinError(#[from] JoinError),

    #[error("Error parsing str")]
    ParseStrError,

    #[error("Error while parsing: {0}")]
    ParseIntError(#[from] ParseIntError),

    #[error("Error while parsing: {0}")]
    ParseBoolError(#[from] ParseBoolError),

    #[error("Rkyv error: {0}")]
    Rkyv(#[from] rkyv::rancor::Error),

    #[error("Failed to send a connect request to the tracker")]
    MagnetError(#[from] magnet_url::MagnetError),

    #[error("String is not UTF-8")]
    Utf8Error(#[from] std::string::FromUtf8Error),

    #[error("Failed to decode or encode the bencode buffer")]
    BencodeError,

    #[error("IO error")]
    IO(#[from] io::Error),

    #[error("Tracker resolved to no unusable addresses")]
    TrackerNoHosts,

    #[error("Peer resolved to no unusable addresses")]
    TrackerSocketAddr,

    #[error("The response received from the connect handshake was wrong")]
    TrackerResponse,

    #[error("Received a buffer with an unexpected length.")]
    ResponseLen,

    #[error(
        "Can't parse compact ip list because it's not divisible by the ip \
         version."
    )]
    CompactPeerListRemainder,

    #[error("Could not connect to the UDP socket of the tracker")]
    TrackerSocketConnect,

    #[error("Error when reading magnet link")]
    MagnetLinkInvalid,

    #[error("The response received from the peer is wrong")]
    MessageResponse,

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

    #[error("This torrent is already downloaded fully")]
    TorrentComplete,

    #[error("Could not find torrent for the given info_hash")]
    TorrentDoesNotExist,

    #[error("The piece downloaded does not have a valid hash")]
    PieceInvalid,

    #[error("The peer closed the socket")]
    PeerClosedSocket,

    #[error(
        "Your magnet doesn't have any UDP tracker, for now only those are \
         supported."
    )]
    NoUDPTracker,

    #[error("Could not send message to Disk")]
    SendErrorDisk,

    #[error("Could not send message to Daemon: {0}")]
    SendDaemonError(String),

    #[error("Could not receive message from oneshot")]
    ReceiveErrorOneshot(#[from] oneshot::error::RecvError),

    #[error("Could not send message to Peer")]
    SendErrorPeer(#[from] mpsc::error::SendError<PeerMsg>),

    #[error("Could not send message to UI")]
    SendErrorTorrent(#[from] mpsc::error::SendError<TorrentMsg>),

    #[error("Could not send message to TCP socket")]
    SendErrorTcp,

    #[error("You cannot add a duplicate torrent, only 1 is allowed")]
    NoDuplicateTorrent,

    #[error("Can't have duplicate peers")]
    NoDuplicatePeer,

    #[error("No peers in the torrent")]
    NoPeers,
}
