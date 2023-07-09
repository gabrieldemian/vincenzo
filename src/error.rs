use std::io;

use thiserror::Error;

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
    #[error("Could not open the file")]
    FileOpenError,
    #[error("This torrent is already downloaded fully")]
    TorrentComplete,
    #[error("The piece downloaded does not have a valid hash")]
    PieceInvalid,
    #[error("The peer took to long to respond")]
    Timeout,
}
