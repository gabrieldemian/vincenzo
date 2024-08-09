use clap::Parser;

#[derive(Parser, Debug, Default)]
#[clap(name = "Vincenzo", author = "Gabriel Lombardo")]
#[command(author, version, about, long_about = None)]
pub struct Args {
    /// Download a torrent using it's magnet link, wrapped in quotes.
    #[clap(short, long)]
    pub magnet: Option<String>,

    /// Print all torrent status on stdout
    #[clap(short, long)]
    pub stats: bool,

    /// Pause a torrent given a hash string of its id
    #[clap(short, long)]
    pub pause: Option<String>,

    /// Stop all torrents and gracefully shutdown
    #[clap(short, long)]
    pub quit: bool,
    //     /// If the program should quit after all torrents are fully
    // downloaded     #[clap(short, long)]
    //     pub quit_after_complete: bool,
}
