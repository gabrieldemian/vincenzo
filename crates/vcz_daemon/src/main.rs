use clap::Parser;
use futures::SinkExt;
use tokio::net::{TcpListener, TcpStream};
use tokio_util::codec::Framed;
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

    let is_daemon_running =
        TcpListener::bind(listen.unwrap_or("127.0.0.1:3030".parse().unwrap())).await.is_err();
    println!("is running {is_daemon_running:?} {:?}", listen);

    // if the daemon is already running,
    // we dont want to run it again,
    // we just want to run the CLI flags.
    if !is_daemon_running {
        let mut daemon = Daemon::new(download_dir).await;

        if let Some(listen) = listen {
            daemon.config.listen = listen;
        }

        println!("Daemon running on: {:?}", daemon.config.listen);
        daemon.run().await.unwrap();
    }

    // send messages to daemon depending on the flags passed
    let socket = TcpStream::connect(listen.unwrap_or("127.0.0.1:3030".parse().unwrap()))
        .await
        .unwrap();

    let mut socket = Framed::new(socket, DaemonCodec);

    if let Some(magnet) = args.magnet {
        println!("sending magnet {magnet:?}");
        socket.send(Message::NewTorrent(magnet)).await.unwrap();
    }

    if args.quit {
        socket.send(Message::Quit).await.unwrap();
    }
}
