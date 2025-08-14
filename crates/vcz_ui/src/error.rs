use std::net::SocketAddr;

use thiserror::Error;
use tokio::sync::mpsc;

use crate::{action::Action, tui::Event};

#[derive(Error, Debug)]
pub enum Error {
    #[error("Could not send message to UI")]
    SendErrorFr,

    #[error("IO error")]
    IO(#[from] std::io::Error),

    #[error("Daemon Error")]
    DaemonError(#[from] vincenzo::error::Error),

    #[error("Could not send message to TCP socket: `{0}`")]
    SendErrorTcp(String),

    #[error("Could not send message")]
    SendError(#[from] mpsc::error::SendError<Event>),

    #[error("Could not send action")]
    SendErrorAction(#[from] mpsc::error::SendError<Action>),

    #[error("Could not receive message")]
    RecvError,

    #[error("The daemon is not running on the given addr: {0}")]
    DaemonNotRunning(SocketAddr),
}
