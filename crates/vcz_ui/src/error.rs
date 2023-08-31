use thiserror::Error;
use tokio::sync::mpsc;

use crate::FrMsg;

#[derive(Error, Debug)]
pub enum Error {
    #[error("Could not send message to UI")]
    SendErrorFr(#[from] mpsc::error::SendError<FrMsg>),
}
