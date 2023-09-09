use directories::ProjectDirs;
use std::net::SocketAddr;
use std::path::PathBuf;
use tokio::io::AsyncReadExt;

use serde::Deserialize;
use serde::Serialize;
use tokio::fs::create_dir_all;
use tokio::fs::File;
use tokio::fs::OpenOptions;

use crate::error::Error;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Config {
    pub download_dir: String,
    pub daemon_addr: Option<SocketAddr>,
}

impl Config {
    async fn config_file() -> Result<(File, PathBuf), Error> {
        // load the config file
        let dotfile = ProjectDirs::from("", "", "Vincenzo").ok_or(Error::HomeInvalid)?;
        let mut config_path = dotfile.config_dir().to_path_buf();

        if !config_path.exists() {
            create_dir_all(&config_path)
                .await
                .map_err(|_| Error::FolderOpenError(config_path.to_str().unwrap().to_owned()))?
        }

        config_path.push("config.toml");

        Ok((
            OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .open(&config_path)
                .await
                .expect("Error while trying to open the project config folder, please make sure this program has the right permissions.") ,
            config_path.into()
        ))
    }

    /// Load the configuration file and transform it into Self.
    /// If the file does not exist, it tries to create the file
    /// with the default configurations.
    pub async fn load() -> Result<Self, Error> {
        let mut str = String::new();
        let (mut file, _) = Self::config_file().await?;
        file.read_to_string(&mut str).await.unwrap();
        let config = toml::from_str::<Config>(&str).map_err(|_| Error::ConfigDeserializeError)?;
        Ok(config)
    }
}
