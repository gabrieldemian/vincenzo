//! A daemon that runs on the background and handles everything
//! that is not the UI.
use futures::{SinkExt, StreamExt};
use hashbrown::HashMap;
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::Arc,
    time::Duration,
};
use tokio_util::codec::Framed;
use tracing::{error, info, trace, warn};

use tokio::{
    net::{TcpListener, TcpStream},
    select, spawn,
    sync::{mpsc, oneshot, RwLock},
    time::interval,
};

use crate::{
    config::Config,
    daemon_wire::{DaemonCodec, Message},
    disk::{Disk, DiskMsg},
    error::Error,
    extensions::{ExtDataTrait, ExtTrait},
    magnet::Magnet,
    peer::PeerId,
    torrent::{InfoHash, Torrent, TorrentMsg, TorrentState, TorrentStatus},
    utils::to_human_readable,
};

/// The daemon is the highest-level entity in the library.
/// It owns [`Disk`] and [`Torrent`]s, which owns Peers.
///
/// The communication with the daemon happens via TCP with messages
/// documented at [`DaemonCodec`].
///
/// The daemon is decoupled from the UI and can even run on different machines,
/// and so, they need a way to communicate. We use TCP, so we can benefit
/// from the Framed utilities that tokio provides, making it easy
/// to create a protocol for the Daemon. HTTP wastes more bandwith
/// and would reduce consistency since the BitTorrent protocol nowadays rarely
/// uses HTTP.
pub struct Daemon {
    pub disk_tx: Option<mpsc::Sender<DiskMsg>>,
    pub ctx: Arc<DaemonCtx>,
    pub torrent_txs: HashMap<InfoHash, mpsc::Sender<TorrentMsg>>,

    // u8 is the extension ID
    pub exts: HashMap<u8, Box<dyn ExtTrait<Msg = Message>>>,
    pub ext_data: HashMap<PeerId, HashMap<u8, Box<dyn ExtDataTrait>>>,

    rx: mpsc::Receiver<DaemonMsg>,
}

/// Context of the [`Daemon`] that may be shared between other types.
pub struct DaemonCtx {
    pub tx: mpsc::Sender<DaemonMsg>,
    /// States of all Torrents, updated each second by the Torrent struct.
    pub torrent_states: RwLock<HashMap<InfoHash, TorrentState>>,
}

/// Messages used by the [`Daemon`] for internal communication.
/// All of these local messages have an equivalent remote message
/// on [`DaemonMsg`].
#[derive(Debug)]
pub enum DaemonMsg {
    /// Tell Daemon to add a new torrent and it will immediately
    /// announce to a tracker, connect to the peers, and start the download.
    NewTorrent(Magnet),
    /// Message that the Daemon will send to all connectors when the state
    /// of a torrent updates (every 1 second).
    TorrentState(TorrentState),
    /// Ask the Daemon to send a [`TorrentState`] of the torrent with the given
    RequestTorrentState(InfoHash, oneshot::Sender<Option<TorrentState>>),
    /// Pause/Resume a torrent.
    TogglePause(InfoHash),
    /// Gracefully shutdown the Daemon
    Quit,
    /// Print the status of all Torrents to stdout
    PrintTorrentStatus,
}

impl Default for Daemon {
    fn default() -> Self {
        Self::new()
    }
}

impl Daemon {
    pub const DEFAULT_LISTENER: SocketAddr =
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 3030);

    /// Initialize the Daemon struct with the default [`DaemonConfig`].
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel::<DaemonMsg>(300);

        Self {
            exts: HashMap::new(),
            ext_data: HashMap::new(),
            rx,
            disk_tx: None,
            torrent_txs: HashMap::new(),
            ctx: Arc::new(DaemonCtx {
                tx,
                torrent_states: RwLock::new(HashMap::new()),
            }),
        }
    }

    /// This function will listen to 3 different event loops:
    /// - The daemon internal messages via MPSC [`DaemonMsg`] (external)
    /// - The daemon TCP framed messages [`DaemonCodec`] (internal)
    /// - The Disk event loop [`Disk`]
    ///
    /// # Important
    ///
    /// Both internal and external messages share the same API.
    /// When the daemon receives a TCP message, it forwards to the
    /// mpsc event loop.
    ///
    /// This is useful to keep consistency, because the same command
    /// that can be fired remotely (via TCP),
    /// can also be fired internaly (via CLI flags).
    pub async fn run(&mut self) -> Result<(), Error> {
        let config = Config::load()?;
        let socket = TcpListener::bind(config.daemon_addr).await.unwrap();

        let (disk_tx, disk_rx) = mpsc::channel::<DiskMsg>(300);
        self.disk_tx = Some(disk_tx);

        let mut disk = Disk::new(disk_rx, config.download_dir);

        spawn(async move {
            let _ = disk.run().await;
        });

        let ctx = self.ctx.clone();

        info!("Daemon listening on: {}", config.daemon_addr);

        // Listen to remote TCP messages
        let handle = spawn(async move {
            loop {
                match socket.accept().await {
                    Ok((socket, addr)) => {
                        info!("Connected with remote: {addr}");

                        let ctx = ctx.clone();

                        spawn(async move {
                            let socket = Framed::new(socket, DaemonCodec);
                            let _ = Self::listen_remote_msgs(socket, ctx).await;
                        });
                    }
                    Err(e) => {
                        error!("Could not connect with remote: {e:#?}");
                    }
                }
            }
        });

        let ctx = self.ctx.clone();

        // Listen to internal mpsc messages
        loop {
            select! {
                Some(msg) = self.rx.recv() => {
                    match msg {
                        DaemonMsg::TorrentState(torrent_state) => {
                            let mut torrent_states = self.ctx.torrent_states.write().await;

                            torrent_states.insert(torrent_state.info_hash.clone(), torrent_state.clone());

                            if config.quit_after_complete && torrent_states.values().all(|v| v.status == TorrentStatus::Seeding) {
                                let _ = ctx.tx.send(DaemonMsg::Quit).await;
                            }

                            drop(torrent_states);
                        }
                        DaemonMsg::NewTorrent(magnet) => {
                            let _ = self.new_torrent(magnet).await;
                        }
                        DaemonMsg::TogglePause(info_hash) => {
                            let _ = self.toggle_pause(&info_hash).await;
                        }
                        DaemonMsg::RequestTorrentState(info_hash, recipient) => {
                            let torrent_states = self.ctx.torrent_states.read().await;
                            let torrent_state = torrent_states.get(&info_hash);
                            let _ = recipient.send(torrent_state.cloned());
                        }
                        DaemonMsg::PrintTorrentStatus => {
                            let torrent_states = self.ctx.torrent_states.read().await;

                            println!("Showing stats of {} torrents.", torrent_states.len());

                            for state in torrent_states.values() {
                                let status_line: String = match state.status {
                                    TorrentStatus::Downloading => {
                                        format!(
                                            "{} - {}",
                                            to_human_readable(state.downloaded as f64),
                                            to_human_readable(state.download_rate as f64),
                                        )
                                    }
                                    _ => state.status.clone().into()
                                };

                                println!(
                                    "\n{}\n{}\nSeeders {} Leechers {}\n{status_line}",
                                    state.name,
                                    to_human_readable(state.size as f64),
                                    state.stats.seeders,
                                    state.stats.leechers,
                                );
                            }
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
        trace!("daemon listen_msgs");

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
                            trace!("daemon received NewTorrent {magnet_link}");
                            let magnet = Magnet::new(&magnet_link);
                            if let Ok(magnet) = magnet {
                                let _ = ctx.tx.send(DaemonMsg::NewTorrent(magnet)).await;
                            }
                        }
                        Message::RequestTorrentState(info_hash) => {
                            trace!("daemon RequestTorrentState {info_hash:?}");
                            let (tx, rx) = oneshot::channel();
                            let _ = ctx.tx.send(DaemonMsg::RequestTorrentState(info_hash, tx)).await;
                            let r = rx.await?;

                            let _ = sink.send(Message::TorrentState(r)).await;
                        }
                        Message::TogglePause(id) => {
                            trace!("daemon received TogglePause {id:?}");
                            let _ = ctx.tx.send(DaemonMsg::TogglePause(id)).await;
                        }
                        Message::Quit => {
                            info!("Daemon is quitting");
                            let _ = ctx.tx.send(DaemonMsg::Quit).await;
                        }
                        Message::PrintTorrentStatus => {
                            trace!("daemon received PrintTorrentStatus");
                            let _ = ctx.tx.send(DaemonMsg::PrintTorrentStatus).await;
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

    /// Pause/resume the torrent, making the download an upload stale.
    pub async fn toggle_pause(
        &self,
        info_hash: &InfoHash,
    ) -> Result<(), Error> {
        let tx = self
            .torrent_txs
            .get(info_hash)
            .ok_or(Error::TorrentDoesNotExist)?;

        tx.send(TorrentMsg::TogglePause).await?;

        Ok(())
    }

    /// Sends a Draw message to the [`UI`] with the updated state of a torrent.
    async fn draw<T>(sink: &mut T, ctx: Arc<DaemonCtx>) -> Result<(), Error>
    where
        T: SinkExt<Message> + Sized + std::marker::Unpin + Send,
    {
        let torrent_states = ctx.torrent_states.read().await;

        for state in torrent_states.values().cloned() {
            // debug!("{state:#?}");
            sink.send(Message::TorrentState(Some(state)))
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
    /// This fn will panic if it is being called BEFORE run
    pub async fn new_torrent(&mut self, magnet: Magnet) -> Result<(), Error> {
        trace!("magnet: {}", *magnet);
        let info_hash = magnet.parse_xt_infohash();

        let mut torrent_states = self.ctx.torrent_states.write().await;

        if torrent_states.get(&info_hash).is_some() {
            warn!("This torrent is already present on the Daemon");
            return Err(Error::NoDuplicateTorrent);
        }

        let torrent_state = TorrentState {
            name: magnet.parse_dn(),
            info_hash: info_hash.clone(),
            ..Default::default()
        };

        torrent_states.insert(info_hash.clone(), torrent_state);
        drop(torrent_states);

        // disk_tx is not None at this point, this is safe
        // (if calling after run)
        let disk_tx = self.disk_tx.clone().unwrap();
        let mut torrent = Torrent::new(disk_tx, self.ctx.tx.clone(), magnet);

        self.torrent_txs.insert(info_hash, torrent.ctx.tx.clone());
        info!("Downloading torrent: {}", torrent.name);

        spawn(async move {
            torrent.start_and_run(None).await?;
            Ok::<(), Error>(())
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
        let _ = self
            .disk_tx
            .as_ref()
            .map(|tx| async { tx.send(DiskMsg::Quit).await });
        Ok(())
    }
}
