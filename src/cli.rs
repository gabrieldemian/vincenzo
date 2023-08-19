use std::net::SocketAddr;

use clap::Parser;

#[derive(Parser, Debug, Default)]
#[clap(
    name = "Vincenzo, a BitTorrent client for your terminal",
    author = "Gabriel Lombardo"
)]
#[command(author, version, about, long_about = None)]
pub struct Args {
    #[clap(short, long)]
    pub download_dir: Option<String>,

    /// The magnet link of the torrent, wrapped in quotes.
    #[clap(short, long)]
    pub magnet: Option<String>,

    /// The socket address on which to listen for new connections.
    #[clap(short, long)]
    pub listen: Option<SocketAddr>,

    #[clap(short, long)]
    pub quit_after_complete: bool,
}
