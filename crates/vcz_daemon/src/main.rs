use clap::Parser;
use futures::SinkExt;
use tokio::net::{TcpListener, TcpStream};
use tokio_util::codec::Framed;
use tracing::Level;
use tracing_subscriber::FmtSubscriber;
use vincenzo::{
    config::Config, daemon::{Args, Daemon}, daemon_wire::{DaemonCodec, Message}
};
mod args;

#[tokio::main]
async fn main() {
    let args = Args::parse();
    let config = Config::load().unwrap();

    let download_dir = args.download_dir.unwrap_or(config.download_dir.clone());
    let mut listen = config.daemon_addr;

    if args.daemon_addr.is_some() {
        listen = args.daemon_addr;
    }

    let is_daemon_running =
        TcpListener::bind(listen.unwrap_or(Daemon::DEFAULT_LISTENER))
            .await
            .is_err();

    // if the daemon is not running, run it
    if !is_daemon_running {
        let subscriber = FmtSubscriber::builder()
            .with_max_level(Level::INFO)
            .without_time()
            .finish();

        tracing::subscriber::set_global_default(subscriber)
            .expect("setting default subscriber failed");

        let mut daemon = Daemon::new(download_dir);

        if let Some(listen) = listen {
            daemon.config.listen = listen;
        }

        daemon.run().await.unwrap();
    }

    // Now that the daemon is running on a process,
    // the user can send commands using CLI flags,
    // using a different terminal, and we want
    // to listen to these flags and send messages to Daemon.
    //
    // 1. Create a TCP connection to Daemon
    let socket = TcpStream::connect(listen.unwrap_or(Daemon::DEFAULT_LISTENER))
        .await
        .unwrap();

    let mut socket = Framed::new(socket, DaemonCodec);

    // 2. Fire the corresponding message of a CLI flag.
    //
    // add a a new torrent to Daemon
    if let Some(magnet) = args.magnet {
        socket.send(Message::NewTorrent(magnet)).await.unwrap();
    }

    if args.stats {
        socket.send(Message::PrintTorrentStatus).await.unwrap();
    }
}
