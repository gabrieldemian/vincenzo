use futures::SinkExt;
use magnet_url::Magnet;
use tokio::{
    net::{TcpListener, TcpStream},
    spawn,
};
use tokio_util::codec::Framed;
use tracing::Level;
use tracing_subscriber::FmtSubscriber;
use vincenzo::{
    config::CONFIG,
    daemon::Daemon,
    daemon_wire::{DaemonCodec, Message},
    disk::Disk,
    error::Error,
};

#[tokio::main]
async fn main() -> Result<(), Error> {
    let subscriber = FmtSubscriber::builder()
        .without_time()
        .with_target(false)
        .with_file(false)
        .with_max_level(Level::INFO)
        .finish();

    tracing::subscriber::set_global_default(subscriber)
        .expect("setting default subscriber failed");

    tracing::info!("config: {:?}", *CONFIG);

    if CONFIG.max_global_peers == 0 || CONFIG.max_torrent_peers == 0 {
        return Err(Error::ConfigError(
            "max_global_peers or max_torrent_peers cannot be zero".into(),
        ));
    }

    if CONFIG.max_global_peers < CONFIG.max_torrent_peers {
        return Err(Error::ConfigError(
            "max_global_peers cannot be less than max_torrent_peers".into(),
        ));
    }

    let is_daemon_running =
        TcpListener::bind(CONFIG.daemon_addr).await.is_err();

    // if the daemon is not running, run it
    if !is_daemon_running {
        let mut disk = Disk::new(CONFIG.download_dir.clone());
        let disk_tx = disk.tx.clone();
        let free_tx = disk.free_tx.clone();

        spawn(async move {
            let _ = disk.run().await;
        });

        let mut daemon = Daemon::new(disk_tx, free_tx);
        daemon.run().await?;
    }

    // Now that the daemon is running on a process,
    // the user can send commands using CLI flags,
    // using a different terminal, and we want
    // to listen to these flags and send messages to Daemon.
    //
    // 1. Create a TCP connection to Daemon
    let Ok(socket) = TcpStream::connect(CONFIG.daemon_addr).await else {
        return Ok(());
    };

    let mut socket = Framed::new(socket, DaemonCodec);

    // 2. Fire the corresponding message of a CLI flag.
    //
    // add a a new torrent to Daemon
    if let Some(magnet) = &CONFIG.magnet {
        let magnet = Magnet::new(magnet)?;
        socket.send(Message::NewTorrent(magnet)).await?;
    }

    if CONFIG.stats {
        socket.send(Message::PrintTorrentStatus).await?;
    }

    if CONFIG.quit {
        socket.send(Message::Quit).await?;
    }

    if let Some(id) = &CONFIG.pause {
        let id = hex::decode(id);
        if let Ok(id) = id {
            socket.send(Message::TogglePause(id.try_into().unwrap())).await?;
        }
    }

    Ok(())
}
