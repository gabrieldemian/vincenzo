use std::net::SocketAddr;

use clap::Parser;
use tokio::sync::mpsc;

use tracing::debug;
use vincenzo::{config::Config, error::Error};

use vcz_ui::{UIMsg, UI};

#[derive(Parser, Debug, Default)]
#[clap(name = "Vincenzo Frontend", author = "Gabriel Lombardo")]
#[command(author, version, about)]
struct Args {
    /// The address that the Daemon is listening on.
    #[clap(short, long)]
    pub daemon_addr: Option<SocketAddr>,
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    // Start and run the terminal UI
    let (fr_tx, fr_rx) = mpsc::channel::<UIMsg>(300);
    let mut fr = UI::new(fr_tx.clone());

    // UI is detached from the Daemon
    fr.is_detached = true;

    let args = Args::parse();
    let config = Config::load()?;

    let daemon_addr = args.daemon_addr.unwrap_or(
        config.daemon_addr.unwrap_or("127.0.0.1:3030".parse().unwrap()),
    );

    fr.run(fr_rx, daemon_addr).await.unwrap();
    debug!("ui exited run");

    Ok(())
}
