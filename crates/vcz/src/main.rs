#![allow(missing_docs)]
#![allow(rustdoc::missing_doc_code_examples)]

use clap::Parser;
use tokio::{runtime::Runtime, spawn, sync::mpsc};

use vcz_daemon::Daemon;
use vcz_lib::{cli::Args, error::Error};

use vcz_lib::FrMsg;
use vcz_ui::Frontend;

#[tokio::main]
async fn main() -> Result<(), Error> {
    let mut daemon = Daemon::new().await.unwrap();

    let rt = Runtime::new().unwrap();
    let handle = std::thread::spawn(move || {
        rt.block_on(async {
            daemon.run().await.unwrap();
        });
    });

    // Start and run the terminal UI
    let (fr_tx, fr_rx) = mpsc::channel::<FrMsg>(300);
    let mut fr = Frontend::new(fr_tx.clone());

    spawn(async move {
        fr.run(fr_rx).await.unwrap();
    });

    let args = Args::parse();

    // If the user passed a magnet through the CLI,
    // start this torrent immediately
    if let Some(magnet) = args.magnet {
        fr_tx.send(FrMsg::NewTorrent(magnet)).await.unwrap();
    }

    handle.join().unwrap();

    Ok(())
}
