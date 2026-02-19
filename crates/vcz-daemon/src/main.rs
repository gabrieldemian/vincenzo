use futures::SinkExt;
use magnet_url::Magnet;
use std::sync::Arc;
use tokio::{
    net::{TcpListener, TcpStream},
    spawn,
    sync::mpsc,
};
use tokio_util::codec::Framed;
use tracing::Level;
use tracing_subscriber::FmtSubscriber;
use vcz_lib::{
    DISK_MSG_BOUND,
    config::Config,
    daemon::Daemon,
    daemon_wire::{DaemonCodec, Message},
    disk::{Disk, DiskMsg, ReturnToDisk},
    error::Error,
};

#[tokio::main]
async fn main() -> Result<(), Error> {
    let config = Arc::new(Config::load()?);

    let subscriber = FmtSubscriber::builder()
        .without_time()
        .with_target(false)
        .with_file(false)
        .with_max_level(Level::INFO)
        .finish();

    tracing::subscriber::set_global_default(subscriber)
        .expect("setting default subscriber failed");

    tracing::info!("config: {config:?}");

    let is_daemon_running =
        TcpListener::bind(config.daemon_addr).await.is_err();

    // if the daemon is not running, run it
    if !is_daemon_running {
        let (disk_tx, disk_rx) = mpsc::channel::<DiskMsg>(DISK_MSG_BOUND);
        let (free_tx, free_rx) = mpsc::unbounded_channel::<ReturnToDisk>();

        let mut daemon = Daemon::new(config.clone(), disk_tx.clone(), free_tx);
        let mut disk = Disk::new(
            config.clone(),
            daemon.ctx.clone(),
            disk_tx,
            disk_rx,
            free_rx,
        );

        let disk_handle = spawn(async move { disk.run().await });
        let daemon_handle = spawn(async move { daemon.run().await });
        disk_handle.await??;
        daemon_handle.await??;
    }

    // Now that the daemon is running on a process,
    // the user can send commands using CLI flags,
    // using a different terminal, and we want
    // to listen to these flags and send messages to Daemon.
    //
    // 1. Create a TCP connection to Daemon
    let Ok(socket) = TcpStream::connect(config.daemon_addr).await else {
        return Ok(());
    };

    let mut socket = Framed::new(socket, DaemonCodec);

    // 2. Fire the corresponding message of a CLI flag.
    //
    // add a a new torrent to Daemon
    if let Some(magnet) = &config.magnet {
        let magnet = Magnet::new(magnet)?;
        socket.send(Message::NewTorrent(magnet)).await?;
    }

    if config.stats {
        socket.send(Message::PrintTorrentStatus).await?;
    }

    if config.quit {
        socket.send(Message::Quit).await?;
    }

    if let Some(id) = &config.pause {
        let id = hex::decode(id);
        if let Ok(id) = id {
            socket.send(Message::TogglePause(id.try_into().unwrap())).await?;
        }
    }

    Ok(())
}
