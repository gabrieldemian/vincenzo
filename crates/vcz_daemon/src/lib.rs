// #![allow(missing_docs)]
// #![allow(rustdoc::missing_doc_code_examples)]
mod error;
use futures::{SinkExt, StreamExt};
use hashbrown::HashMap;
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tokio_util::codec::Framed;

use error::Error;
use tokio::{
    fs::{create_dir_all, File, OpenOptions},
    io::AsyncWriteExt,
    net::{TcpListener, TcpStream},
    select, spawn,
    time::interval,
};

use clap::Parser;
use directories::{ProjectDirs, UserDirs};
use tokio::io::AsyncReadExt;
use tracing_subscriber::prelude::__tracing_subscriber_SubscriberExt;
use vcz_lib::{
    cli::Args,
    config::Config,
    daemon_wire::{DaemonCodec, Message},
    TorrentState,
};

/// The UI will ask the Daemon to create new Torrents.
/// The Daemon will periodically send Draw messages to the UI.
///
/// Daemon also stores `TorrentInfo`s, this is only used
/// by the UI to present data about Torrents.
///
/// Daemon -> Torrent -> Peer
///        -> Torrent -> Peer
///                   -> Peer
pub struct Daemon {
    pub config: Config,
    pub download_dir: String,
    torrent_infos: HashMap<[u8; 20], TorrentState>,
}

impl Daemon {
    pub async fn new() -> Result<Self, Error> {
        let console_layer = console_subscriber::spawn();
        let r = tracing_subscriber::registry();
        r.with(console_layer);

        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open("log.txt")
            .expect("Failed to open log file");

        tracing_subscriber::fmt()
            .with_env_filter("tokio=trace,runtime=trace")
            .with_max_level(tracing::Level::INFO)
            .with_target(false)
            .with_writer(file)
            .compact()
            .with_file(false)
            .without_time()
            .init();

        let args = Args::parse();

        let mut str = String::new();
        let (mut file, _) = Self::config_file().await?;

        file.read_to_string(&mut str).await.unwrap();

        let config = toml::from_str::<Config>(&str);

        let config = config.unwrap();

        let download_dir = args.download_dir.unwrap_or(config.download_dir.clone());

        if !Path::new(&download_dir).exists() {
            return Err(Error::FolderOpenError(download_dir.clone()));
        }

        Ok(Self {
            config,
            download_dir,
            torrent_infos: HashMap::new(),
        })
    }

    pub async fn run(&self) -> Result<(), Error> {
        let socket = TcpListener::bind("127.0.0.1:3030").await.unwrap();

        spawn(async move {
            loop {
                if let Ok((socket, _addr)) = socket.accept().await {
                    spawn(async move {
                        let socket = Framed::new(socket, DaemonCodec);
                        Self::listen_msgs(socket).await?;
                        Ok::<(), Error>(())
                    });
                }
            }
        });

        Ok(())
    }

    async fn listen_msgs(socket: Framed<TcpStream, DaemonCodec>) -> Result<(), Error> {
        let (mut sink, mut stream) = socket.split();
        let mut draw_interval = interval(Duration::from_secs(1));

        loop {
            select! {
                Some(Ok(msg)) = stream.next() => {
                    match msg {
                        Message::NewTorrent(magnet_link) => {
                            // self.new_torrent(&magnet_link).await?;
                        }
                        _ => {}
                    }
                }
                _ = draw_interval.tick() => {
                    // sink.send(Message::TorrentsState(())).await?;
                }
            }
        }
    }

    pub async fn new_torrent(&self, magnet: &str) -> Result<(), Error> {
        Ok(())
    }

    pub async fn config_file() -> Result<(File, PathBuf), Error> {
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

    pub async fn prepare_config() -> Result<(), Error> {
        // load the config file
        let (mut file, config_path) = Self::config_file().await?;

        let mut str = String::new();
        file.read_to_string(&mut str).await.unwrap();
        let config = toml::from_str::<Config>(&str);

        // the user does not have the config file, write
        // the default config
        if config.is_err() {
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
                ))
                .unwrap()
                .to_str()
                .unwrap()
                .into();

            let config_local = Config {
                download_dir,
                listen: None,
            };

            let config_str = toml::to_string(&config_local).unwrap();

            file.write_all(config_str.as_bytes()).await.unwrap();
        }

        Ok(())
    }
}
