use thiserror::Error;

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
    #[error("Invalid magnet: `{0}")]
    InvalidMagnet(String),
    #[error("You cannot add a duplicate torrent, only 1 allowed")]
    NoDuplicateTorrent,
    #[error("Could not send message to TCP socket")]
    SendErrorTcp,
    #[error("Config error: `{0}`")]
    ConfigError(String),
}
