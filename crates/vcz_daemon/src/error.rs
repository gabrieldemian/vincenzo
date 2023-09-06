use std::io;

use thiserror::Error;
// use tokio::sync::{mpsc, oneshot};

#[derive(Error, Debug)]
pub enum Error {
    #[error("The `{0}` folder was not found, please edit the config file manually at `{1}")]
    FolderNotFound(String, String),
    #[error(
        "Could not open the folder `{0}`. Please make sure the program has permission to open it and that the folder exist"
    )]
    FolderOpenError(String),
    #[error("Tried to load $HOME but could not find it. Please make sure you have a $HOME env and that this program has the permission to create dirs.")]
    HomeInvalid,
    #[error("IO error `{0}`")]
    Io(#[from] io::Error),
}
