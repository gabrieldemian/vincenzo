use std::io;

use thiserror::Error;
use tokio::sync::{mpsc, oneshot};

use crate::{disk::DiskMsg, peer::PeerMsg, torrent::TorrentMsg, tracker::TrackerMsg};

#[derive(Error, Debug)]
pub enum Error {
    #[error("Failed to send a connect request to the tracker")]
    ConnectSendFailed,
    #[error("Failed to decode or encode the bencode buffer")]
    BencodeError,
    #[error("Failed to resolve socket address")]
    PeerSocketAddrs(#[from] io::Error),
    #[error("Peer resolved to no unusable addresses")]
    PeerSocketAddr,
    #[error("Tracker resolved to no unusable addresses")]
    TrackerNoHosts,
    #[error("Peer resolved to no unusable addresses")]
    TrackerSocketAddr,
    #[error("The response received from the connect handshake was wrong")]
    TrackerResponse,
    #[error("Tracker event only goes from 0..=2 and a different value was used")]
    TrackerEvent,
    #[error("Tried to call announce without calling connect first")]
    TrackerResponseLength,
    #[error("The response length received from the tracker was less then 20 bytes, when it should be larger")]
    TrackerNoConnectionId,
    #[error("The peer list returned by the announce request is not valid")]
    TrackerCompactPeerList,
    #[error("Could not connect to the UDP socket of the tracker")]
    TrackerSocketConnect,
    #[error("Error when serializing/deserializing")]
    SpeedyError(#[from] speedy::Error),
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
    #[error(
        "Could not open the file `{0}`. Please make sure the program has permission to access it"
    )]
    FileOpenError(String),
    #[error(
        "Could not open the folder `{0}`. Please make sure the program has permission to open it and that the folder exist"
    )]
    FolderOpenError(String),
    #[error("This torrent is already downloaded fully")]
    TorrentComplete,
    #[error("Could not find torrent for the given info_hash")]
    TorrentDoesNotExist,
    #[error("The piece downloaded does not have a valid hash")]
    PieceInvalid,
    #[error("The peer ID does not exist on this torrent")]
    PeerIdInvalid,
    #[error("Disk does not have the provided info_hash")]
    InfoHashInvalid,
    #[error("The peer took to long to respond")]
    Timeout,
    #[error("Your magnet does not have a tracker. Currently, this client does not support DHT, you need to use a magnet that has a tracker.")]
    MagnetNoTracker,
    #[error(
        "Your magnet does not have an info_hash, are you sure you copied the entire magnet link?"
    )]
    MagnetNoInfoHash,
    #[error("Could not send message to Disk")]
    SendErrorDisk(#[from] mpsc::error::SendError<DiskMsg>),
    #[error("Could not receive message from oneshot")]
    ReceiveErrorOneshot(#[from] oneshot::error::RecvError),
    #[error("Could not send message to Peer")]
    SendErrorPeer(#[from] mpsc::error::SendError<PeerMsg>),
    #[error("Could not send message to Tracker")]
    SendErrorTracker(#[from] mpsc::error::SendError<TrackerMsg>),
    #[error("Could not send message to Frontend")]
    SendErrorTorrent(#[from] mpsc::error::SendError<TorrentMsg>),
    #[error("The given PATH is invalid")]
    PathInvalid,
}
