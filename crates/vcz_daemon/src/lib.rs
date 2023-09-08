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
use tracing::debug;

use error::Error;
use tokio::{
    fs::{create_dir_all, File, OpenOptions},
    io::AsyncWriteExt,
    net::{TcpListener, TcpStream},
    select, spawn,
    sync::mpsc,
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
    disk::{Disk, DiskMsg},
    magnet_parser::{get_info_hash, get_magnet},
    torrent::{Torrent, TorrentMsg},
    DaemonMsg, TorrentState,
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
    pub disk_tx: Option<mpsc::Sender<DiskMsg>>,
    tx: mpsc::Sender<DaemonMsg>,
    /// key: info_hash
    torrent_states: HashMap<[u8; 20], TorrentState>,
    /// key: info_hash
    torrent_txs: HashMap<[u8; 20], mpsc::Sender<TorrentMsg>>,
}

impl Daemon {
    pub async fn new() -> Result<Self, Error> {
        let (tx, _rx) = mpsc::channel::<DaemonMsg>(300);

        let console_layer = console_subscriber::spawn();
        let r = tracing_subscriber::registry();
        r.with(console_layer);

        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open("../../log.txt")
            .expect("Failed to open log file");

        tracing_subscriber::fmt()
            .with_env_filter("tokio=trace,runtime=trace")
            .with_max_level(tracing::Level::DEBUG)
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
            tx,
            disk_tx: None,
            config,
            download_dir,
            torrent_states: HashMap::new(),
            torrent_txs: HashMap::new(),
        })
    }

    /// Run the daemon event loop and disk event loop.
    pub async fn run(&mut self) -> Result<(), Error> {
        let socket = TcpListener::bind("127.0.0.1:3030").await.unwrap();

        let (disk_tx, disk_rx) = mpsc::channel::<DiskMsg>(300);
        self.disk_tx = Some(disk_tx);

        let mut disk = Disk::new(disk_rx, self.download_dir.clone());
        spawn(async move {
            disk.run().await.unwrap();
        });

        'outer: loop {
            if let Ok((socket, _addr)) = socket.accept().await {
                let socket = Framed::new(socket, DaemonCodec);
                self.listen_msgs(socket).await?;
                break 'outer;
            }
        }
        Ok(())
    }

    /// Listen to messages sent by the UI
    async fn listen_msgs(&mut self, socket: Framed<TcpStream, DaemonCodec>) -> Result<(), Error> {
        debug!("daemon listen_msgs");
        let (mut sink, mut stream) = socket.split();
        let mut draw_interval = interval(Duration::from_secs(1));

        'outer: loop {
            select! {
                Some(Ok(msg)) = stream.next() => {
                    match msg {
                        Message::NewTorrent(magnet_link) => {
                            debug!("daemon received newTorrent");
                            self.new_torrent(&magnet_link).await?;
                            // immediately draw after adding the new torrent,
                            // we dont want to wait up to 1 second to update the UI.
                            // because of the `draw_interval`.
                            self.draw(&mut sink).await?;
                        }
                        Message::Quit => {
                            debug!("daemon received quit");
                            self.quit().await?;
                            break 'outer;
                        }
                        _ => {}
                    }
                }
                _ = draw_interval.tick() => {
                    self.draw(&mut sink).await?;
                }
            }
        }
        Ok(())
    }

    pub async fn new_torrent(&mut self, magnet: &str) -> Result<(), Error> {
        let magnet = get_magnet(magnet).map_err(|_| Error::InvalidMagnet(magnet.to_owned()))?;
        let info_hash = get_info_hash(&magnet.xt.clone().unwrap());

        if self.torrent_states.get(&info_hash).is_some() {
            return Err(Error::NoDuplicateTorrent);
        }

        let name = magnet.dn.clone().unwrap_or("Unknown".to_owned());

        let torrent_state = TorrentState {
            name,
            ..Default::default()
        };

        self.torrent_states.insert(info_hash, torrent_state);

        let args = Args::parse();
        let mut listen = self.config.listen;

        if args.listen.is_some() {
            listen = args.listen;
        }

        // disk_tx is not None at this point, this is safe
        // (if calling after run)
        let disk_tx = self.disk_tx.clone().unwrap();
        let mut torrent = Torrent::new(disk_tx, self.tx.clone(), magnet);

        self.torrent_txs.insert(info_hash, torrent.ctx.tx.clone());

        spawn(async move {
            torrent.start_and_run(listen).await.unwrap();
            torrent.disk_tx.send(DiskMsg::Quit).await.unwrap();
        });

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

    /// Sends a Draw message to the UI with all the torrent states
    async fn draw<T>(&self, sink: &mut T) -> Result<(), Error>
    where
        T: SinkExt<Message> + Sized + std::marker::Unpin + Send,
    {
        debug!("daemon sending draw");
        for state in self.torrent_states.values().cloned() {
            sink.send(Message::TorrentState(state))
                .await
                .map_err(|_| Error::SendErrorTcp)?;
        }
        Ok(())
    }

    async fn quit(&mut self) -> Result<(), Error> {
        // tell all torrents that we are gracefully shutting down,
        // each torrent will kill their peers tasks, and their tracker task
        for (_, tx) in std::mem::take(&mut self.torrent_txs) {
            spawn(async move {
                let _ = tx.send(TorrentMsg::Quit).await;
            });
        }
        let _ = self.disk_tx.as_ref().unwrap().send(DiskMsg::Quit).await;
        Ok(())
    }
}
