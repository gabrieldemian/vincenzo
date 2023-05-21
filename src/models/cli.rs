use clap::Parser;

#[derive(Parser, Debug)]
#[clap(name = "Bittorrent CLI in Rust")]
#[command(author, version, about, long_about = None)]
pub struct Args {
    #[clap(short, long, default_value = "~/Downloads")]
    pub download_folder: String,

    #[clap(short, long)]
    pub magnet: Option<String>,
}
