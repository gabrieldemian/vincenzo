use thiserror::Error;
use tokio::sync::mpsc;
use vcz_lib::DaemonMsg;

// use crate::DaemonMsg;

#[derive(Error, Debug)]
pub enum Error {
    #[error("Could not send message to UI")]
    SendErrorFr(#[from] mpsc::error::SendError<DaemonMsg>),
    #[error("Could not send message to TCP socket: `{0}`")]
    SendErrorTcp(String),
}
