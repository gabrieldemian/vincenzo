use clap::Parser;
use futures::SinkExt;
use tokio::{
    net::{TcpListener, TcpStream},
    spawn,
};
use tokio_util::codec::Framed;
use tracing::Level;
use tracing_subscriber::FmtSubscriber;
use vincenzo::{
    args::Args,
    config::Config,
    daemon::Daemon,
    daemon_wire::{DaemonCodec, Message},
    disk::Disk,
    error::Error,
};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Error> {
    let args = Args::parse();
    let config = Config::load()?;

    if config.max_global_peers == 0 || config.max_torrent_peers == 0 {
        return Err(Error::ConfigError(
            "max_global_peers or max_torrent_peers cannot be zero".into(),
        ));
    }

    if config.max_global_peers < config.max_torrent_peers {
        return Err(Error::ConfigError(
            "max_global_peers cannot be less than max_torrent_peers".into(),
        ));
    }

    let daemon_addr = config.daemon_addr;

    let is_daemon_running = TcpListener::bind(daemon_addr).await.is_err();

    // if the daemon is not running, run it
    if !is_daemon_running {
        let subscriber = FmtSubscriber::builder()
            .with_max_level(Level::INFO)
            .without_time()
            .finish();

        tracing::subscriber::set_global_default(subscriber)
            .expect("setting default subscriber failed");

        let mut disk = Disk::new(config.download_dir.clone());
        let disk_tx = disk.tx.clone();

        spawn(async move {
            let _ = disk.run().await;
        });

        let mut daemon = Daemon::new(disk_tx);

        daemon.run().await?;
    }

    // Now that the daemon is running on a process,
    // the user can send commands using CLI flags,
    // using a different terminal, and we want
    // to listen to these flags and send messages to Daemon.
    //
    // 1. Create a TCP connection to Daemon
    let socket = TcpStream::connect(daemon_addr).await?;

    let mut socket = Framed::new(socket, DaemonCodec);

    // 2. Fire the corresponding message of a CLI flag.
    //
    // add a a new torrent to Daemon
    if let Some(magnet) = args.magnet {
        socket.send(Message::NewTorrent(magnet)).await?;
    }

    if args.stats {
        socket.send(Message::PrintTorrentStatus).await?;
    }

    if args.quit {
        socket.send(Message::Quit).await?;
    }

    if let Some(id) = args.pause {
        let id = hex::decode(id);
        if let Ok(id) = id {
            socket.send(Message::TogglePause(id.try_into().unwrap())).await?;
        }
    }

    Ok(())
}
