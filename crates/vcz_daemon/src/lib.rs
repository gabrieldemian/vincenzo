// #![allow(missing_docs)]
// #![allow(rustdoc::missing_doc_code_examples)]
mod error;
use futures::{SinkExt, StreamExt};
use hashbrown::HashMap;
use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio_util::codec::Framed;
use tracing::{debug, warn};

use error::Error;
use tokio::{
    fs::{create_dir_all, File, OpenOptions},
    io::AsyncWriteExt,
    net::{TcpListener, TcpStream},
    select, spawn,
    sync::{mpsc, RwLock},
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
    rx: Option<mpsc::Receiver<DaemonMsg>>,
    /// key: info_hash
    torrent_txs: HashMap<[u8; 20], mpsc::Sender<TorrentMsg>>,
    ctx: Arc<DaemonCtx>,
    /// Addr of who is connected to the Daemon,
    /// only one connection is allowed.
    remote_addr: Option<SocketAddr>,
}

pub struct DaemonCtx {
    pub tx: mpsc::Sender<DaemonMsg>,
    /// key: info_hash
    pub torrent_states: RwLock<HashMap<[u8; 20], TorrentState>>,
}

impl Daemon {
    pub async fn new() -> Result<Self, Error> {
        let (tx, rx) = mpsc::channel::<DaemonMsg>(300);

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

        let config = Config::load()
            .await
            .expect("Could not get the configuration file");

        let download_dir = args.download_dir.unwrap_or(config.download_dir.clone());

        if !Path::new(&download_dir).exists() {
            return Err(Error::FolderOpenError(download_dir.clone()));
        }

        Ok(Self {
            remote_addr: None,
            rx: Some(rx),
            disk_tx: None,
            config,
            download_dir,
            torrent_txs: HashMap::new(),
            ctx: Arc::new(DaemonCtx {
                tx,
                torrent_states: RwLock::new(HashMap::new()),
            }),
        })
    }

    /// Run the daemon event loop and disk event loop.
    pub async fn run(&mut self) -> Result<(), Error> {
        let args = Args::parse();
        let mut listen = self.config.daemon_addr;

        if args.daemon_addr.is_some() {
            listen = args.daemon_addr;
        }

        if listen.is_none() {
            // if the user did not pass a `daemon_addr` on the config or CLI,
            // we default to this value
            listen = Some("127.0.0.1:3030".parse().unwrap())
        }

        let socket = TcpListener::bind(listen.unwrap()).await.unwrap();

        let (disk_tx, disk_rx) = mpsc::channel::<DiskMsg>(300);
        self.disk_tx = Some(disk_tx);

        let mut disk = Disk::new(disk_rx, self.download_dir.clone());
        spawn(async move {
            disk.run().await.unwrap();
        });

        let rx = std::mem::take(&mut self.rx).unwrap();

        // spawn(async move {
        //
        // });

        'outer: loop {
            if let Ok((socket, addr)) = socket.accept().await {
                // if self.remote_addr.is_some() {
                //     eprintln!("{addr} Tried to connect to Daemon, but it already has one active connection, and only one connection is allowed");
                //     warn!("{addr} Tried to connect to Daemon, but it already has one active connection, and only one connection is allowed");
                // } else {
                self.remote_addr = Some(addr);

                let socket = Framed::new(socket, DaemonCodec);

                self.listen_remote_msgs(socket, self.ctx.tx.clone(), rx)
                    .await?;

                break 'outer;
                // }
            }
        }

        Ok(())
    }

    /// Listen to messages sent remotely via TCP, like the UI sending messages to Daemon.
    async fn listen_remote_msgs(
        &mut self,
        socket: Framed<TcpStream, DaemonCodec>,
        tx: mpsc::Sender<DaemonMsg>,
        mut rx: mpsc::Receiver<DaemonMsg>,
    ) -> Result<(), Error> {
        debug!("daemon listen_msgs");

        let mut draw_interval = interval(Duration::from_secs(1));
        let (mut sink, mut stream) = socket.split();

        'outer: loop {
            select! {
                // listen to messages sent remotely via TCP, and pass them
                // to our rx, the `Some` branch right below this one.
                // We do this so we can use the exact same messages
                // when sent remotely via TCP (i.e UI on remote server),
                // or locally on the same binary (i.e CLI).
                Some(Ok(msg)) = stream.next() => {
                    match msg {
                        Message::NewTorrent(magnet_link) => {
                            debug!("daemon received newTorrent");
                            let _ = tx.send(DaemonMsg::NewTorrent(magnet_link)).await;
                        }
                        Message::Quit => {
                            debug!("daemon received quit");
                            let _ = tx.send(DaemonMsg::Quit).await;
                        }
                        _ => {}
                    }
                }
                // listen to messages sent locally, from the daemon binary.
                // a Torrent that is owned by the Daemon, may send messages to this channel
                Some(msg) = rx.recv() => {
                    match msg {
                        DaemonMsg::TorrentState(torrent_state) => {
                            let mut torrent_states = self.ctx.torrent_states.write().await;

                            torrent_states.insert(torrent_state.info_hash.clone(), torrent_state.clone());

                            drop(torrent_states);
                        }
                        DaemonMsg::NewTorrent(magnet) => {
                            self.new_torrent(&magnet).await?;
                            // immediately draw after adding the new torrent,
                            // we dont want to wait up to 1 second to update the UI.
                            // because of the `draw_interval`.
                            self.draw(&mut sink).await?;
                        }
                        DaemonMsg::Quit => {
                            self.quit().await?;
                            break 'outer;
                        }
                    }
                }
                _ = draw_interval.tick() => {
                    if self.remote_addr.is_some() {
                        let r = self.draw(&mut sink).await;
                        if r.is_err() {
                            self.remote_addr = None;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    pub async fn new_torrent(&mut self, magnet: &str) -> Result<(), Error> {
        let magnet = get_magnet(magnet).map_err(|_| Error::InvalidMagnet(magnet.to_owned()))?;
        let info_hash = get_info_hash(&magnet.xt.clone().unwrap());

        let mut torrent_states = self.ctx.torrent_states.write().await;

        if torrent_states.get(&info_hash).is_some() {
            return Err(Error::NoDuplicateTorrent);
        }

        let name = magnet.dn.clone().unwrap_or("Unknown".to_owned());

        let torrent_state = TorrentState {
            name,
            info_hash,
            ..Default::default()
        };

        torrent_states.insert(info_hash, torrent_state);
        drop(torrent_states);

        // disk_tx is not None at this point, this is safe
        // (if calling after run)
        let disk_tx = self.disk_tx.clone().unwrap();
        let mut torrent = Torrent::new(disk_tx, self.ctx.tx.clone(), magnet);

        self.torrent_txs.insert(info_hash, torrent.ctx.tx.clone());

        spawn(async move {
            torrent.start_and_run(None).await.unwrap();
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

        // if the user does not have the config file,
        // write the default config
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
                daemon_addr: None,
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
        let torrent_states = self.ctx.torrent_states.read().await;

        for state in torrent_states.values().cloned() {
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
