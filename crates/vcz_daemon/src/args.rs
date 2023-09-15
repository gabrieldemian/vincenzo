use std::net::SocketAddr;

use clap::Parser;

#[derive(Parser, Debug, Default)]
#[clap(name = "Vincenzo Daemon", author = "Gabriel Lombardo")]
#[command(author, version, about, long_about = None)]
pub(crate) struct Args {
    /// The Daemon will accept TCP connections on this address.
    #[clap(short, long)]
    pub listen: Option<SocketAddr>,

    /// The directory in which torrents will be downloaded
    #[clap(short, long)]
    pub download_dir: Option<String>,

    /// Download a torrent using it's magnet link, wrapped in quotes.
    #[clap(short, long)]
    pub magnet: Option<String>,

    /// If the program should quit after all torrents are fully downloaded
    #[clap(short, long)]
    pub quit_after_complete: bool,

    /// Immediately kills the process without announcing to any tracker
    #[clap(short, long)]
    pub kill: bool,
}
