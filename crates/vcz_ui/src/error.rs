use thiserror::Error;
use tokio::sync::mpsc;

use crate::tui::Event;

#[derive(Error, Debug)]
pub enum Error {
    #[error("Could not send message to UI")]
    SendErrorFr,

    #[error("Could not send message to TCP socket: `{0}`")]
    SendErrorTcp(String),

    #[error("Could not send message")]
    SendError(#[from] mpsc::error::SendError<Event>),

    #[error("Could not receive message")]
    RecvError,

    #[error("Daemon socket was closed")]
    IoError(#[from] std::io::Error),
}
