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
use tokio_util::{codec::Framed, sync::CancellationToken};
use tracing::{debug, info, trace, warn};

use tokio::{
    net::{TcpListener, TcpStream},
    select, spawn,
    sync::{mpsc, oneshot},
    task::JoinHandle,
    time::interval,
};

use crate::{
    config::CONFIG,
    daemon_wire::{DaemonCodec, Message},
    disk::{DiskMsg, ReturnToDisk},
    error::Error,
    magnet::Magnet,
    peer::{self, Peer, PeerId},
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
    pub metadata_sizes: HashMap<InfoHash, Option<usize>>,

    /// Connected peers of all torrents
    connected_peers: u32,

    local_peer_handle: Option<JoinHandle<()>>,

    /// States of all Torrents, updated each second by the Torrent struct.
    torrent_states: Vec<TorrentState>,
    rx: mpsc::Receiver<DaemonMsg>,
}

/// Context of the [`Daemon`] that may be shared between other types.
pub struct DaemonCtx {
    pub tx: mpsc::Sender<DaemonMsg>,
    pub free_tx: mpsc::UnboundedSender<ReturnToDisk>,
    pub local_peer_id: PeerId,
}

/// Messages used by the [`Daemon`] for internal communication.
/// All of these local messages have an equivalent remote message
/// on [`DaemonMsg`].
#[derive(Debug)]
pub enum DaemonMsg {
    /// Tell Daemon to add a new torrent and it will immediately
    /// announce to a tracker, connect to the peers, and start the download.
    NewTorrent(magnet_url::Magnet),

    GetConnectedPeers(oneshot::Sender<u32>),
    GetMetadataSize(oneshot::Sender<Option<usize>>, InfoHash),
    SetMetadataSize(usize, InfoHash),

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
    pub fn new(
        disk_tx: mpsc::Sender<DiskMsg>,
        free_tx: mpsc::UnboundedSender<ReturnToDisk>,
    ) -> Self {
        let (tx, rx) = mpsc::channel::<DaemonMsg>(100);

        let local_peer_id = PeerId::generate();

        Self {
            connected_peers: 0,
            metadata_sizes: HashMap::default(),
            disk_tx,
            rx,
            local_peer_handle: None,
            torrent_ctxs: HashMap::new(),
            torrent_states: Vec::new(),
            ctx: Arc::new(DaemonCtx { tx, local_peer_id, free_tx }),
        }
    }

    pub async fn run_local_peer(&mut self) -> Result<JoinHandle<()>, Error> {
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
        Ok(spawn(async move {
            debug!("accepting requests in {local_socket:?}");

            loop {
                select! {
                    Ok((socket, addr)) = local_socket.accept() => {
                        info!("received inbound connection from {addr}");

                        let daemon_ctx = daemon_ctx.clone();

                        spawn(async move {
                            let idle_peer = Peer::<peer::Idle>::new();

                            let mut connected_peer =
                                idle_peer.inbound_handshake(socket, daemon_ctx).await?;

                            if let Err(r) = connected_peer.run().await {
                                warn!(
                                    "{} peer loop stopped due to an error: {r:?}",
                                    connected_peer.state.ctx.remote_addr
                                );
                                connected_peer.free_pending_blocks();
                                return Err(r);
                            }

                            Ok::<(), Error>(())
                        });
                    }
                }
            }
        }))
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

    async fn delete_torrent(
        &mut self,
        info_hash: &InfoHash,
    ) -> Result<(), Error> {
        let Some(ctx) = self.torrent_ctxs.get(info_hash) else {
            return Ok(());
        };
        let _ = ctx.tx.send(TorrentMsg::Quit).await;
        let _ =
            ctx.disk_tx.send(DiskMsg::DeleteTorrent(info_hash.clone())).await;
        self.torrent_states.retain(|v| *v.info_hash != **info_hash);
        self.torrent_ctxs.remove(info_hash);
        Ok(())
    }

    async fn delete_all_torrents(&mut self) -> Result<(), Error> {
        for ctx in self.torrent_ctxs.values() {
            let _ = ctx.tx.send(TorrentMsg::Quit).await;
            let _ = ctx
                .disk_tx
                .send(DiskMsg::DeleteTorrent(ctx.info_hash.clone()))
                .await;
        }
        self.torrent_ctxs.clear();
        Ok(())
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

        self.local_peer_handle = Some(self.run_local_peer().await?);

        #[cfg(feature = "test")]
        self.add_test_torrents().await;

        let signals = Signals::new([SIGHUP, SIGTERM, SIGINT, SIGQUIT])?;
        let handle = signals.handle();
        let signals_task =
            tokio::spawn(Daemon::handle_signals(signals, ctx.tx.clone()));

        let mut test_interval = interval(Duration::from_secs(1));

        // token to cancel all frontend's tasks
        let all_fr_token = CancellationToken::new();

        'outer: loop {
            select! {
                _ = test_interval.tick() => {
                    #[cfg(feature = "test")]
                    self.tick_test().await;
                }
                Ok((socket, addr)) = socket.accept() => {
                    info!("connected to remote: {addr}");

                    let socket = Framed::new(socket, DaemonCodec);
                    let (mut sink, mut stream) = socket.split();

                    let ctx = ctx.clone();
                    let all_fr_token = all_fr_token.clone();
                    let fr_token = CancellationToken::new();

                    tokio::spawn(async move {
                        let mut draw_interval = interval(Duration::from_secs(1));
                        let ctx = ctx.clone();

                        'inner: loop {
                            select! {
                                _ = all_fr_token.cancelled() => {
                                    info!("disconnected from remote: {addr}");
                                    sink.close().await?;
                                    break 'inner;
                                }
                                _ = fr_token.cancelled() => {
                                    info!("disconnected from remote: {addr}");
                                    sink.close().await?;
                                    break 'inner;
                                }
                                _ = draw_interval.tick() => {
                                    let (otx, orx) = oneshot::channel();

                                    ctx.tx.send(DaemonMsg::GetAllTorrentStates(otx)).await?;

                                    if sink.send(Message::TorrentStates(orx.await?)).await
                                        .map_err(|_| Error::SendErrorTcp).is_err()
                                    {
                                        fr_token.cancel();
                                    }
                                }
                                Some(Ok(msg)) = stream.next() => {
                                    if msg == Message::FrontendQuit {
                                        fr_token.cancel();
                                    }
                                    Self::handle_remote_msgs(&ctx.tx, msg, &mut sink).await?;
                                }
                                else => fr_token.cancel(),
                            }
                        }
                        Ok::<(), Error>(())
                    });
                }
                // Listen to internal mpsc messages
                Some(msg) = self.rx.recv() => {
                    match msg {
                        DaemonMsg::GetTorrentCtx(tx, info_hash) => {
                            let ctx = self.torrent_ctxs.get(&info_hash).cloned();
                            let _ = tx.send(ctx);
                        }
                        DaemonMsg::GetMetadataSize(tx, info_hash) => {
                            let Some(metadata_size) =
                                self.metadata_sizes.get(&info_hash).cloned()
                            else { continue } ;
                            let _ = tx.send(metadata_size);
                        }
                        DaemonMsg::SetMetadataSize(metadata, info_hash) => {
                            let Some(metadata_size) =
                                self.metadata_sizes.get_mut(&info_hash)
                            else { continue };
                            *metadata_size = Some(metadata);
                        }
                        DaemonMsg::GetConnectedPeers(tx) => {
                            let _ = tx.send(self.connected_peers);
                        }
                        DaemonMsg::DeleteTorrent(info_hash) => {
                            info!("deleting torrent {info_hash:?}");
                            let _ = self.delete_torrent(&info_hash).await;
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
                                            to_human_readable(state.downloaded as f64),
                                            state.download_rate,
                                        )
                                    }
                                    _ => state.status.into()
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
                            let _ = self.delete_all_torrents().await;
                            let _ = self.disk_tx.send(DiskMsg::Quit).await;
                            signals_task.abort();
                            if let Some(h) = &self.local_peer_handle {
                                h.abort();
                            }
                            handle.close();
                            all_fr_token.cancel();
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
                let _ = tx.send(DaemonMsg::DeleteTorrent(info_hash)).await;
            }
            Message::NewTorrent(magnet) => {
                info!("new_torrent: {:?}", magnet.hash());

                let _ = tx.send(DaemonMsg::NewTorrent(magnet)).await;
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
    pub async fn new_torrent(
        &mut self,
        magnet: magnet_url::Magnet,
    ) -> Result<(), Error> {
        let magnet: Magnet = magnet.into();
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

        let torrent = Torrent::new(
            self.disk_tx.clone(),
            self.ctx.free_tx.clone(),
            self.ctx.clone(),
            magnet,
        );

        self.torrent_ctxs.insert(info_hash, torrent.ctx.clone());

        spawn(async move {
            let mut torrent = torrent.start().await?;
            torrent.run().await?;
            Ok::<(), Error>(())
        });

        Ok(())
    }

    #[cfg(feature = "test")]
    async fn add_test_torrents(&mut self) {
        use crate::torrent::Stats;
        self.torrent_states.extend([
            TorrentState {
                name: "Test torrent 01".to_string(),
                status: TorrentStatus::Downloading,
                stats: Stats { leechers: 4, seeders: 35, interval: 1000 },
                connected_peers: 25,
                downloading_from: 5,
                have_info: true,
                download_rate: 15_000,
                downloaded: 0,
                size: 150_000_000_000,
                info_hash: InfoHash::random(),
                ..Default::default()
            },
            TorrentState {
                name: "Test torrent 02".to_string(),
                status: TorrentStatus::Seeding,
                stats: Stats { leechers: 4, seeders: 35, interval: 1000 },
                connected_peers: 30,
                downloading_from: 3,
                have_info: true,
                download_rate: 0,
                downloaded: 150_000_000,
                size: 150_000_000,
                info_hash: InfoHash::random(),
                ..Default::default()
            },
            TorrentState {
                name: "Test torrent 03".to_string(),
                status: TorrentStatus::Downloading,
                stats: Stats { leechers: 1, seeders: 9, interval: 1000 },
                connected_peers: 25,
                downloading_from: 3,
                have_info: true,
                download_rate: 12_000,
                downloaded: 100,
                size: 180_327_100_000,
                info_hash: InfoHash::random(),
                ..Default::default()
            },
            TorrentState {
                name: "Test torrent 04".to_string(),
                status: TorrentStatus::Downloading,
                stats: Stats { leechers: 1, seeders: 9, interval: 1000 },
                connected_peers: 25,
                downloading_from: 3,
                have_info: true,
                download_rate: 12_000,
                downloaded: 100,
                size: 180_327_100_000,
                info_hash: InfoHash::random(),
                ..Default::default()
            },
            TorrentState {
                name: "Test torrent 05".to_string(),
                status: TorrentStatus::Downloading,
                stats: Stats { leechers: 1, seeders: 9, interval: 1000 },
                connected_peers: 25,
                downloading_from: 3,
                have_info: true,
                download_rate: 12_000,
                downloaded: 100,
                size: 180_327_100_000,
                info_hash: InfoHash::random(),
                ..Default::default()
            },
            TorrentState {
                name: "Test torrent 06".to_string(),
                status: TorrentStatus::Downloading,
                stats: Stats { leechers: 1, seeders: 9, interval: 1000 },
                connected_peers: 25,
                downloading_from: 3,
                have_info: true,
                download_rate: 12_000,
                downloaded: 100,
                size: 180_327_100_000,
                info_hash: InfoHash::random(),
                ..Default::default()
            },
        ]);
    }

    #[cfg(feature = "test")]
    async fn tick_test(&mut self) {
        let TorrentState {
            download_rate,
            upload_rate,
            uploaded,
            downloaded,
            ..
        } = &mut self.torrent_states[0];
        *download_rate = rand::random_range(30_000..100_000);
        *upload_rate = rand::random_range(20_000..45_000);
        *uploaded += *upload_rate;
        *downloaded += *download_rate;

        let TorrentState {
            download_rate,
            upload_rate,
            uploaded,
            downloaded,
            ..
        } = &mut self.torrent_states[1];
        *upload_rate = rand::random_range(70_000..100_000);
        *download_rate = rand::random_range(0..1_000);
        *uploaded += *upload_rate;
        *downloaded += *download_rate;

        let TorrentState {
            download_rate,
            downloaded,
            upload_rate,
            uploaded,
            ..
        } = &mut self.torrent_states[2];
        *download_rate = rand::random_range(30_000..100_000);
        *downloaded += *download_rate;
        *uploaded += *upload_rate;

        let TorrentState {
            download_rate,
            downloaded,
            upload_rate,
            uploaded,
            ..
        } = &mut self.torrent_states[3];

        *download_rate = rand::random_range(30_000..100_000);
        *downloaded += *download_rate;
        *uploaded += *upload_rate;
        let TorrentState {
            download_rate,
            downloaded,
            upload_rate,
            uploaded,
            ..
        } = &mut self.torrent_states[4];

        *download_rate = rand::random_range(30_000..100_000);
        *downloaded += *download_rate;
        *uploaded += *upload_rate;

        let TorrentState {
            download_rate,
            downloaded,
            upload_rate,
            uploaded,
            ..
        } = &mut self.torrent_states[5];
        *download_rate = rand::random_range(30_000..100_000);
        *downloaded += *download_rate;
        *uploaded += *upload_rate;
    }
}
