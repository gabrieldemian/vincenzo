use std::net::SocketAddr;

use clap::Parser;

#[derive(Parser, Debug, Default)]
#[clap(
    name = "Vincenzo, a BitTorrent client for your terminal",
    author = "Gabriel Lombardo"
)]
#[command(author, version, about, long_about = None)]
pub struct Args {
    /// The path of the folder where to download file. Must be wrapped in quotes and end with '/'.
    #[clap(short, long, default_value = "~/Downloads")]
    pub download_dir: String,

    /// The magnet link of the torrent, wrapped in quotes.
    #[clap(short, long)]
    pub magnet: Option<String>,

    /// A comma separated list of <ip>:<port> pairs of the seeds.
    // #[clap(short, long)]
    // pub seeds: Option<Vec<SocketAddr>>,

    /// The socket address on which to listen for new connections.
    #[clap(short, long)]
    pub listen: Option<SocketAddr>,

    #[clap(short, long)]
    pub quit_after_complete: bool,
}
