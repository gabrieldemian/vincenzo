use std::net::SocketAddr;
use thiserror::Error;
use tokio::sync::mpsc;
use crate::action::Action;

impl From<mpsc::error::SendError<Action>> for Error {
    fn from(_value: mpsc::error::SendError<Action>) -> Self {
        Self::SendErrorAction
    }
}

#[derive(Error, Debug)]
pub enum Error {
    #[error("Could not send message to UI")]
    SendErrorFr,

    #[error("IO error")]
    IO(#[from] std::io::Error),

    #[error("Daemon Error")]
    DaemonError(#[from] vcz_lib::error::Error),

    #[error("Could not send message to TCP socket: `{0}`")]
    SendErrorTcp(String),

    #[error("Could not send action")]
    SendErrorAction,

    #[error("Could not receive message")]
    RecvError,

    #[error("The daemon is not running on the given addr: {0}")]
    DaemonNotRunning(SocketAddr),
}
