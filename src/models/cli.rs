use clap::Parser;

#[derive(Parser, Debug)]
#[clap(name = "p2p chat")]
pub struct Opt {
    #[clap(long)]
    pub download_folder: String,
}
