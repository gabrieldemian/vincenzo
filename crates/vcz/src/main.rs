use clap::Parser;
use tokio::{runtime::Runtime, spawn, sync::mpsc};

use tracing::debug;
use vincenzo::daemon::Daemon;
use vincenzo::{cli::Args, error::Error};

use vcz_ui::{UI, UIMsg};

#[tokio::main]
async fn main() -> Result<(), Error> {
    let mut daemon = Daemon::new().await.unwrap();

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
        fr.run(fr_rx).await.unwrap();
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
