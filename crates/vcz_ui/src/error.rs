use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("Could not send message to UI")]
    SendErrorFr,
    #[error("Could not send message to TCP socket: `{0}`")]
    SendErrorTcp(String),
}
