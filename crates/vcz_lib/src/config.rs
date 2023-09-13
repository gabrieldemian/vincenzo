use directories::ProjectDirs;
use directories::UserDirs;
use std::net::SocketAddr;
use std::path::PathBuf;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;

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
    /// Returns the configuration fs File and it's path,
    ///
    /// If it doesn't exist, we try to create a default configuration file
    /// at the user's config folder, which we get from their environmental variables.
    ///
    /// # Errors
    ///
    /// The fn will try to get the download and config dirs from the users home dir.
    /// This fn can fail if the program does not have access to any path, or file.
    pub async fn config_file() -> Result<(File, PathBuf), Error> {
        // load the config file
        // errors if the user does not have a home folder
        let dotfile = ProjectDirs::from("", "", "Vincenzo").ok_or(Error::HomeInvalid)?;
        let mut config_path = dotfile.config_dir().to_path_buf();

        // If the user has a home folder, but for some reason we cant open it
        if !config_path.exists() {
            create_dir_all(&config_path)
                .await
                .map_err(|_| Error::FolderOpenError(config_path.to_str().unwrap().to_owned()))?
        }

        // check that the user's download dir is valid
        let download_dir = UserDirs::new()
            .ok_or(Error::FolderNotFound(
                "home".into(),
                config_path.to_str().unwrap().to_owned(),
            ))
            .unwrap()
            .download_dir()
            .ok_or(Error::FolderNotFound(
                "download".into(),
                config_path.to_str().unwrap().to_owned(),
            ))?
            .to_str()
            .unwrap()
            .into();

        config_path.push("config.toml");

        // try to open the config file, and create one
        // if it doesnt exist. This will only fail if we dont
        // have permission to read or write to this path.
        let mut config_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(&config_path)
            .await?;

        // read the configuration file, and validate if it is
        // a valid toml file. If it fails, it could also be empty.
        // In both cases we write the default configuration to the file.
        let mut dst = String::new();
        config_file.read_to_string(&mut dst).await?;

        let c = toml::from_str::<Config>(&dst);

        if c.is_err() {
            let default_config = Config {
                download_dir,
                daemon_addr: None,
            };

            let config_str = toml::to_string(&default_config).unwrap();

            config_file.write_all(config_str.as_bytes()).await.unwrap();
        }

        let config_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(&config_path)
            .await?;

        Ok((config_file, config_path.into()))
    }

    /// Load the configuration file and transform it into Self.
    /// If the file does not exist, it tries to create the file
    /// with the default configurations.
    pub async fn load() -> Result<Self, Error> {
        let (mut file, _p) = Self::config_file().await?;

        let mut config_str = String::new();
        file.read_to_string(&mut config_str).await.unwrap();

        let config = toml::from_str::<Config>(&config_str).unwrap();

        Ok(config)
    }
}
