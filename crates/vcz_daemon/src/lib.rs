// #![allow(missing_docs)]
// #![allow(rustdoc::missing_doc_code_examples)]
mod error;
use futures::{SinkExt, StreamExt};
use hashbrown::HashMap;
use std::{sync::Arc, time::Duration};
use tokio_util::codec::Framed;
use tracing::debug;

use error::Error;
use tokio::{
    net::{TcpListener, TcpStream},
    select, spawn,
    sync::{mpsc, RwLock},
    time::interval,
};

use clap::Parser;
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

/// A daemon that runs on the background and handles everything
/// that is not the UI.
///
/// The daemon is the most high-level API in all backend libs.
/// It owns [`Disk`] and [`Torrent`]s, which owns [`Peer`]s.
///
/// The communication with the daemon happens via TCP with messages
/// documented at [`DaemonCodec`].
///
/// The daemon and the UI can run on different machines, and so,
/// they need a way to communicate. We use TCP, so we can benefit
/// from the Framed utilities that tokio provides, making it easy
/// to create a protocol for the Daemon. HTTP wastes more bandwith
/// and would reduce consistency.
pub struct Daemon {
    pub config: Config,
    pub download_dir: String,
    pub disk_tx: Option<mpsc::Sender<DiskMsg>>,
    rx: mpsc::Receiver<DaemonMsg>,
    /// key: info_hash
    torrent_txs: HashMap<[u8; 20], mpsc::Sender<TorrentMsg>>,
    ctx: Arc<DaemonCtx>,
}

pub struct DaemonCtx {
    pub tx: mpsc::Sender<DaemonMsg>,
    /// key: info_hash
    pub torrent_states: RwLock<HashMap<[u8; 20], TorrentState>>,
}

impl Daemon {
    /// Tries to create a Daemon struct and setup the logger listener.
    /// Can error if the configuration validation fails,
    /// check [`Config`] for more details about the validation.
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
            .map_err(|e| Error::ConfigError(e.to_string()))?;

        let download_dir = args.download_dir.unwrap_or(config.download_dir.clone());

        Ok(Self {
            rx,
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

    /// This function will listen to 3 different event loops:
    /// - The daemon internal messages via MPSC [`DaemonMsg`] (external)
    /// - The daemon TCP framed messages [`DaemonCodec`] (internal)
    /// - The Disk event loop [`Disk`]
    ///
    /// Both internal and external messages share the same API.
    /// When the daemon receives a TCP message, it forwards to the
    /// mpsc event loop.
    ///
    /// This is useful to keep consistency, because the same command
    /// that can be fired remotely (example: via TCP),
    /// can also be fired internaly (example: via CLI flags).
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

        let ctx = self.ctx.clone();

        // Listen to remote TCP messages
        let handle = spawn(async move {
            loop {
                if let Ok((socket, _addr)) = socket.accept().await {
                    let ctx = ctx.clone();
                    spawn(async move {
                        let socket = Framed::new(socket, DaemonCodec);
                        let _ = Self::listen_remote_msgs(socket, ctx).await;
                    });
                }
            }
        });

        // Listen to internal mpsc messages
        loop {
            select! {
                Some(msg) = self.rx.recv() => {
                    match msg {
                        DaemonMsg::TorrentState(torrent_state) => {
                            let mut torrent_states = self.ctx.torrent_states.write().await;

                            torrent_states.insert(torrent_state.info_hash.clone(), torrent_state.clone());

                            drop(torrent_states);
                        }
                        DaemonMsg::NewTorrent(magnet) => {
                            let _ = self.new_torrent(&magnet).await;
                            // Self::draw(&mut sink).await?;
                        }
                        DaemonMsg::Quit => {
                            let _ = self.quit().await;
                            handle.abort();
                            break;
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Listen to messages sent remotely via TCP,
    /// A UI can be a standalone binary that is executing on another machine,
    /// and wants to control the daemon using the [`DaemonCodec`] protocol.
    async fn listen_remote_msgs(
        socket: Framed<TcpStream, DaemonCodec>,
        ctx: Arc<DaemonCtx>,
    ) -> Result<(), Error> {
        debug!("daemon listen_msgs");

        let mut draw_interval = interval(Duration::from_secs(1));
        let (mut sink, mut stream) = socket.split();

        loop {
            select! {
                // listen to messages sent remotely via TCP, and pass them
                // to our rx. We do this so we can use the exact same messages
                // when sent remotely via TCP (i.e UI on remote server),
                // or locally on the same binary (i.e CLI).
                Some(Ok(msg)) = stream.next() => {
                    match msg {
                        Message::NewTorrent(magnet_link) => {
                            debug!("daemon received newTorrent");
                            let _ = ctx.tx.send(DaemonMsg::NewTorrent(magnet_link)).await;
                        }
                        Message::Quit => {
                            debug!("daemon received quit");
                            let _ = ctx.tx.send(DaemonMsg::Quit).await;
                        }
                        _ => {}
                    }
                }
                // listen to messages sent locally, from the daemon binary.
                // a Torrent that is owned by the Daemon, may send messages to this channel
                _ = draw_interval.tick() => {
                    let _ = Self::draw(&mut sink, ctx.clone()).await;
                }
            }
        }
    }

    /// Sends a Draw message to the [`UI`] with the updated state of a torrent.
    async fn draw<T>(sink: &mut T, ctx: Arc<DaemonCtx>) -> Result<(), Error>
    where
        T: SinkExt<Message> + Sized + std::marker::Unpin + Send,
    {
        debug!("daemon sending draw");
        let torrent_states = ctx.torrent_states.read().await;

        for state in torrent_states.values().cloned() {
            sink.send(Message::TorrentState(state))
                .await
                .map_err(|_| Error::SendErrorTcp)?;
        }

        drop(torrent_states);
        Ok(())
    }

    /// Create a new [`Torrent`] given a magnet link URL
    /// and run the torrent's event loop.
    ///
    /// # Errors
    ///
    /// This fn may return an [`Err`] if the magnet link is invalid
    ///
    /// # Panic
    ///
    /// This fn will panic if it is being called BEFORE [`run`].
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
        });

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
