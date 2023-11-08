use clap::Parser;
use futures::SinkExt;
use tokio::net::{TcpListener, TcpStream};
use tokio_util::codec::Framed;
use tracing_subscriber::prelude::__tracing_subscriber_SubscriberExt;
use vincenzo::{
    config::Config,
    daemon::{Args, Daemon},
    daemon_wire::{DaemonCodec, Message},
};
mod args;

#[tokio::main]
async fn main() {
    let args = Args::parse();
    let config = Config::load().await.unwrap();

    let download_dir = args.download_dir.unwrap_or(config.download_dir.clone());
    let mut listen = config.daemon_addr;

    if args.daemon_addr.is_some() {
        listen = args.daemon_addr;
    }

    let is_daemon_running = TcpListener::bind(listen.unwrap_or("127.0.0.1:3030".parse().unwrap()))
        .await
        .is_err();

    // if the daemon is already running,
    // we dont want to run it again,
    // we just want to run the CLI flags.
    if !is_daemon_running {
        let console_layer = console_subscriber::spawn();
        let r = tracing_subscriber::registry();
        r.with(console_layer);

        tracing_subscriber::fmt()
            .with_env_filter("tokio=trace,runtime=trace")
            .with_max_level(tracing::Level::INFO)
            .with_target(false)
            .compact()
            .with_file(false)
            .without_time()
            .init();

        let mut daemon = Daemon::new(download_dir);

        if let Some(listen) = listen {
            daemon.config.listen = listen;
        }

        daemon.run().await.unwrap();
    }

    // send messages to daemon depending on the flags passed
    let socket = TcpStream::connect(listen.unwrap_or("127.0.0.1:3030".parse().unwrap()))
        .await
        .unwrap();

    let mut socket = Framed::new(socket, DaemonCodec);

    if let Some(magnet) = args.magnet {
        socket.send(Message::NewTorrent(magnet)).await.unwrap();
    }
}
