use clap::Parser;
use tokio::{runtime::Runtime, spawn, sync::mpsc};

use tracing::debug;
use vincenzo::{config::Config, daemon::Args};
use vincenzo::daemon::Daemon;
use vincenzo::error::Error;

use vcz_ui::{UIMsg, UI};

#[tokio::main]
async fn main() -> Result<(), Error> {
    let args = Args::parse();
    let config = Config::load().await.unwrap();

    let download_dir = args.download_dir.unwrap_or(config.download_dir.clone());
    let daemon_addr = args.daemon_addr.unwrap_or(
        config
            .daemon_addr
            .unwrap_or("127.0.0.1:3030".parse().unwrap()),
    );

    let mut daemon = Daemon::new(download_dir).await;
    daemon.config.listen = daemon_addr;

    let rt = Runtime::new().unwrap();
    let handle = std::thread::spawn(move || {
        rt.block_on(async {
            daemon.run().await.unwrap();
            debug!("daemon exited run");
        });
    });

    // Start and run the terminal UI
    let (fr_tx, fr_rx) = mpsc::channel::<UIMsg>(300);
    let mut fr = UI::new(fr_tx.clone());

    spawn(async move {
        fr.run(fr_rx, daemon_addr).await.unwrap();
        debug!("ui exited run");
    });

    let args = Args::parse();

    // If the user passed a magnet through the CLI,
    // start this torrent immediately
    if let Some(magnet) = args.magnet {
        fr_tx.send(UIMsg::NewTorrent(magnet)).await.unwrap();
    }

    handle.join().unwrap();

    Ok(())
}
