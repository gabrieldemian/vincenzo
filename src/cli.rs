use std::net::SocketAddr;

use clap::Parser;

#[derive(Parser, Debug, Default)]
#[clap(name = "Bittorrent CLI in Rust")]
#[command(author, version, about, long_about = None)]
pub struct Args {
    /// The path of the folder where to download file. Must be wrapped in quotes and end with '/'.
    #[clap(short, long, default_value = "~/Downloads")]
    pub download_dir: String,

    /// The magnet link of the torrent, wrapped in quotes.
    #[clap(short, long)]
    pub magnet: String,

    /// A comma separated list of <ip>:<port> pairs of the seeds.
    #[structopt(short, long)]
    pub seeds: Option<Vec<SocketAddr>>,

    /// The socket address on which to listen for new connections.
    #[structopt(short, long)]
    pub listen: Option<SocketAddr>,

    #[structopt(short, long)]
    pub quit_after_complete: bool,
}
