use std::io;

use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("Failed to send a connect request to the tracker")]
    ConnectSendFailed,
    #[error("Failed to resolve socket address")]
    TrackerSocketAddrs(#[from] io::Error),
    #[error("Tracker resolved to no unusable addresses")]
    TrackerNoHosts,
    #[error("The response received from the connect handshake was wrong")]
    TrackerResponse,
    #[error("Tried to call announce without calling connect first")]
    TrackerResponseLength,
    #[error("The response length received from the tracker was less then 20 bytes, when it should be larger")]
    TrackerNoConnectionId,
    #[error("The peer list returned by the announce request is not valid")]
    TrackerCompactPeerList,
    #[error("Error when serializing/deserializing")]
    SpeedyError(#[from] speedy::Error),
    #[error("Error when reading magnet link")]
    MagnetLinkInvalid,
    #[error("The response received from the peer is wrong")]
    MessageResponse,
    #[error("The request took to long to arrive")]
    RequestTimeout,
}
