//! A daemon that runs on the background and handles everything
//! that is not the UI.
use futures::{
    stream::{SplitSink, StreamExt},
    SinkExt,
};
use hashbrown::HashMap;
use signal_hook::consts::signal::*;
use signal_hook_tokio::Signals;
use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::Arc,
    time::Duration,
};
use tokio_util::codec::Framed;
use tracing::{debug, error, info, trace, warn};

use tokio::{
    net::{TcpListener, TcpStream},
    select, spawn,
    sync::{mpsc, oneshot},
    time::interval,
};

use crate::{
    config::CONFIG,
    daemon_wire::{DaemonCodec, Message},
    disk::DiskMsg,
    error::Error,
    magnet::Magnet,
    peer::PeerId,
    torrent::{
        InfoHash, Torrent, TorrentCtx, TorrentMsg, TorrentState, TorrentStatus,
    },
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
    pub disk_tx: mpsc::Sender<DiskMsg>,
    pub ctx: Arc<DaemonCtx>,
    pub torrent_ctxs: HashMap<InfoHash, Arc<TorrentCtx>>,

    /// Connected peers of all torrents
    connected_peers: u32,

    /// States of all Torrents, updated each second by the Torrent struct.
    torrent_states: Vec<TorrentState>,
    rx: mpsc::Receiver<DaemonMsg>,
}

/// Context of the [`Daemon`] that may be shared between other types.
pub struct DaemonCtx {
    pub tx: mpsc::Sender<DaemonMsg>,
    pub local_peer_id: PeerId,
}

/// Messages used by the [`Daemon`] for internal communication.
/// All of these local messages have an equivalent remote message
/// on [`DaemonMsg`].
#[derive(Debug)]
pub enum DaemonMsg {
    /// Tell Daemon to add a new torrent and it will immediately
    /// announce to a tracker, connect to the peers, and start the download.
    NewTorrent(Magnet),

    GetConnectedPeers(oneshot::Sender<u32>),
    GetTorrentCtx(oneshot::Sender<Option<Arc<TorrentCtx>>>, InfoHash),
    IncrementConnectedPeers,
    DecrementConnectedPeers,
    DeleteTorrent(InfoHash),

    GetAllTorrentStates(oneshot::Sender<Vec<TorrentState>>),

    /// Message that the Daemon will send to all connectors when the state
    /// of a torrent updates (every 1 second).
    TorrentState(TorrentState),

    /// Ask the Daemon to send a [`TorrentState`] of the torrent with the given
    RequestTorrentState(InfoHash, oneshot::Sender<Option<TorrentState>>),

    /// Pause/Resume a torrent.
    TogglePause(InfoHash),

    /// Print the status of all Torrents to stdout
    PrintTorrentStatus,

    /// Gracefully shutdown the Daemon
    Quit,
}

impl Daemon {
    pub const DEFAULT_LISTENER: SocketAddr =
        SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 51411);

    /// Initialize the Daemon struct with the default [`DaemonConfig`].
    pub fn new(disk_tx: mpsc::Sender<DiskMsg>) -> Self {
        let (tx, rx) = mpsc::channel::<DaemonMsg>(100);

        let local_peer_id = PeerId::gen();

        Self {
            connected_peers: 0,
            disk_tx,
            rx,
            torrent_ctxs: HashMap::new(),
            torrent_states: Vec::new(),
            ctx: Arc::new(DaemonCtx { tx, local_peer_id }),
        }
    }

    pub async fn run_local_peer(&self) -> Result<(), Error> {
        let local_addr = SocketAddr::new(
            if CONFIG.is_ipv6 {
                IpAddr::V6(Ipv6Addr::UNSPECIFIED)
            } else {
                IpAddr::V4(Ipv4Addr::UNSPECIFIED)
            },
            CONFIG.local_peer_port,
        );

        info!("local peer listening on: {local_addr}");

        let local_socket = TcpListener::bind(local_addr).await?;
        let daemon_ctx = self.ctx.clone();

        // accept connections from other peers
        spawn(async move {
            debug!("accepting requests in {local_socket:?}");

            loop {
                if let Ok((socket, addr)) = local_socket.accept().await {
                    info!("received inbound connection from {addr}");

                    let daemon_ctx = daemon_ctx.clone();

                    spawn(async move {
                        Torrent::start_and_run_inbound_peer(daemon_ctx, socket)
                            .await?;

                        Ok::<(), Error>(())
                    });
                }
            }
        });
        Ok(())
    }

    async fn handle_signals(mut signals: Signals, tx: mpsc::Sender<DaemonMsg>) {
        while let Some(signal) = signals.next().await {
            match signal {
                SIGHUP => {
                    // Reload configuration
                    // Reopen the log file
                }
                sig @ (SIGTERM | SIGINT | SIGQUIT | SIGKILL) => {
                    info!("received SIG {sig}");
                    let _ = tx.send(DaemonMsg::Quit).await;
                }
                _ => unreachable!(),
            }
        }
    }

    /// This function will listen to 3 different event loops:
    /// - The daemon internal messages via MPSC [`DaemonMsg`]
    /// - The daemon TCP framed messages [`DaemonCodec`]
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
    #[tracing::instrument(name = "daemon", skip_all)]
    pub async fn run(&mut self) -> Result<(), Error> {
        let socket = TcpListener::bind(CONFIG.daemon_addr).await.unwrap();

        info!("listening on: {}", CONFIG.daemon_addr);

        let ctx = self.ctx.clone();

        self.run_local_peer().await?;

        let signals = Signals::new([SIGHUP, SIGTERM, SIGINT, SIGQUIT])?;
        let handle = signals.handle();
        let signals_task =
            tokio::spawn(Daemon::handle_signals(signals, ctx.tx.clone()));

        'outer: loop {
            select! {
                // listen for remote TCP connections
                Ok((socket, addr)) = socket.accept() => {
                    info!("connected to remote: {addr}");

                    let socket = Framed::new(socket, DaemonCodec);
                    let (mut sink, mut stream) = socket.split();

                    let ctx = ctx.clone();

                    tokio::spawn(async move {
                        // listen to messages sent locally, from the daemon binary.
                        // a Torrent that is owned by the Daemon, may send messages to this channel
                        let mut draw_interval = interval(Duration::from_secs(1));
                        let ctx = ctx.clone();

                        'inner: loop {
                            select! {
                                _ = draw_interval.tick() => {
                                    let (otx, orx) = oneshot::channel();
                                    ctx.tx.send(DaemonMsg::GetAllTorrentStates(otx)).await?;

                                    if sink.send(Message::TorrentStates(orx.await?)).await
                                            .map_err(|_| Error::SendErrorTcp).is_err()
                                    {
                                        break 'inner;
                                    }
                                }
                                Some(Ok(msg)) = stream.next() => {
                                    if msg == Message::FrontendQuit {
                                        break 'inner;
                                    }
                                    Self::handle_remote_msgs(&ctx.tx, msg, &mut sink).await?;
                                }
                                else => break 'inner
                            }
                        }

                        info!("disconnected from remote: {addr}");

                        Ok::<(), Error>(())
                    });
                }
                // Listen to internal mpsc messages
                Some(msg) = self.rx.recv() => {
                    match msg {
                        DaemonMsg::GetTorrentCtx(tx, info) => {
                            let ctx = self.torrent_ctxs.get(&info).cloned();
                            let _ = tx.send(ctx);
                        }
                        DaemonMsg::GetConnectedPeers(tx) => {
                            let _ = tx.send(self.connected_peers);
                        }
                        DaemonMsg::DeleteTorrent(info_hash) => {
                            info!("deleting torrent {info_hash:?}");

                            let Some(ctx) = self.torrent_ctxs.get(&info_hash) else {
                                continue
                            };
                            ctx.tx.send(TorrentMsg::Quit).await?;
                            ctx.disk_tx.send(DiskMsg::DeleteTorrent(info_hash.clone())).await?;
                            self.torrent_states.retain(|v| v.info_hash != info_hash);
                        }
                        DaemonMsg::IncrementConnectedPeers => self.connected_peers += 1,
                        DaemonMsg::DecrementConnectedPeers => {
                            if self.connected_peers > 0 {
                                self.connected_peers -= 1;
                            }
                        },
                        DaemonMsg::GetAllTorrentStates(tx) => {
                            let _ = tx.send(self.torrent_states.clone());
                        }
                        DaemonMsg::TorrentState(torrent_state) => {
                            let found =
                                self.torrent_states
                                    .iter_mut()
                                    .find(|v| v.info_hash == torrent_state.info_hash);

                            if let Some(found) = found {
                                *found = torrent_state;
                            } else {
                                self.torrent_states.push(torrent_state);
                            }

                            if CONFIG.quit_after_complete
                                &&
                                self.torrent_states
                                    .iter()
                                    .all(|v| v.status == TorrentStatus::Seeding)
                            {
                                let _ = ctx.tx.send(DaemonMsg::Quit).await;
                            }
                        }
                        DaemonMsg::NewTorrent(magnet) => {
                            let _ = self.new_torrent(magnet).await;
                        }
                        DaemonMsg::TogglePause(info_hash) => {
                            let _ = self.toggle_pause(&info_hash).await;
                        }
                        DaemonMsg::RequestTorrentState(info_hash, recipient) => {
                            let torrent_state = self.torrent_states.iter().find(|v| v.info_hash == info_hash);
                            let _ = recipient.send(torrent_state.cloned());
                        }
                        DaemonMsg::PrintTorrentStatus => {
                            println!("Showing stats of {} torrents.", self.torrent_states.len());

                            for state in &self.torrent_states {
                                let status_line: String = match state.status {
                                    TorrentStatus::Downloading => {
                                        format!(
                                            "{} - {}",
                                            to_human_readable(state.downloaded),
                                            to_human_readable(state.download_rate),
                                        )
                                    }
                                    _ => state.status.clone().into()
                                };

                                println!(
                                    "\n{}\n{}\nSeeders {} Leechers {}\n{status_line}",
                                    state.name,
                                    to_human_readable(state.size),
                                    state.stats.seeders,
                                    state.stats.leechers,
                                );
                            }
                        }
                        DaemonMsg::Quit => {
                            let _ = self.quit_torrents_and_disk().await;
                            handle.close();
                            let _ = signals_task.await;
                            break 'outer;
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
    async fn handle_remote_msgs(
        tx: &mpsc::Sender<DaemonMsg>,
        msg: Message,
        sink: &mut SplitSink<Framed<TcpStream, DaemonCodec>, Message>,
    ) -> Result<(), Error> {
        // listen to messages sent remotely via TCP, and pass them
        // to our rx. We do this so we can use the exact same messages
        // when sent remotely via TCP (i.e UI on remote server),
        // or locally on the same binary (i.e CLI).
        match msg {
            Message::DeleteTorrent(info_hash) => {
                tx.send(DaemonMsg::DeleteTorrent(info_hash)).await?;
            }
            Message::NewTorrent(magnet_link) => {
                info!("received new_torrent: {magnet_link}");

                let magnet = Magnet::new(&magnet_link);

                if let Ok(magnet) = magnet {
                    let _ = tx.send(DaemonMsg::NewTorrent(magnet)).await;
                } else {
                    error!("invalid magnet link");
                }
            }
            Message::GetTorrentState(info_hash) => {
                trace!("daemon RequestTorrentState {info_hash:?}");

                let (otx, orx) = oneshot::channel();

                let _ = tx
                    .send(DaemonMsg::RequestTorrentState(info_hash, otx))
                    .await;

                if let Some(r) = orx.await? {
                    let _ = sink.send(Message::TorrentState(r)).await;
                }
            }
            Message::TogglePause(id) => {
                trace!("daemon received TogglePause {id:?}");
                let _ = tx.send(DaemonMsg::TogglePause(id)).await;
            }
            Message::Quit => {
                info!("Daemon is quitting");
                let _ = tx.send(DaemonMsg::Quit).await;
            }
            Message::PrintTorrentStatus => {
                trace!("daemon received PrintTorrentStatus");
                let _ = tx.send(DaemonMsg::PrintTorrentStatus).await;
            }
            _ => {}
        };

        Ok(())
    }

    /// Pause/resume the torrent, making the download an upload stale.
    pub async fn toggle_pause(
        &self,
        info_hash: &InfoHash,
    ) -> Result<(), Error> {
        let ctx = self
            .torrent_ctxs
            .get(info_hash)
            .ok_or(Error::TorrentDoesNotExist)?;

        ctx.tx.send(TorrentMsg::TogglePause).await?;

        Ok(())
    }

    /// Create a new [`Torrent`] given a magnet link URL
    /// and run the torrent's event loop.
    pub async fn new_torrent(&mut self, magnet: Magnet) -> Result<(), Error> {
        let info_hash = magnet.parse_xt_infohash();

        if self.torrent_states.iter().any(|v| v.info_hash == info_hash) {
            warn!("this torrent is already present");
            return Err(Error::NoDuplicateTorrent);
        }

        let torrent_state = TorrentState {
            name: magnet.parse_dn(),
            info_hash: info_hash.clone(),
            ..Default::default()
        };

        self.torrent_states.push(torrent_state);

        let torrent =
            Torrent::new(self.disk_tx.clone(), self.ctx.clone(), magnet);

        self.torrent_ctxs.insert(info_hash, torrent.ctx.clone());

        spawn(async move {
            let mut torrent = torrent.start().await?;
            torrent.run().await?;
            Ok::<(), Error>(())
        });

        Ok(())
    }

    async fn quit_torrents_and_disk(&mut self) {
        // tell all torrents that we are quitting the client,
        // each torrent will kill their peers tasks, and their tracker task
        for (_, ctx) in std::mem::take(&mut self.torrent_ctxs) {
            //     spawn(async move {
            let _ = ctx.tx.send(TorrentMsg::Quit).await;
            //     });
        }

        let _ = self.disk_tx.send(DiskMsg::Quit).await;
    }
}
