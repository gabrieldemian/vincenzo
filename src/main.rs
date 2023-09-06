#![allow(missing_docs)]
#![allow(rustdoc::missing_doc_code_examples)]
use std::path::Path;

use tokio::{
    fs::{create_dir_all, OpenOptions},
    io::AsyncWriteExt,
};

use clap::Parser;
use directories::{ProjectDirs, UserDirs};
use tokio::{io::AsyncReadExt, runtime::Runtime, spawn, sync::mpsc};
use tracing_subscriber::prelude::__tracing_subscriber_SubscriberExt;
use vcz_lib::{
    cli::Args,
    config::Config,
    disk::{Disk, DiskMsg},
    error::Error,
    // frontend::{FrMsg, Frontend},
};

#[tokio::main]
async fn main() -> Result<(), Error> {
    let daemon = Daemon::new();

    let rt = Runtime::new().unwrap();
    let handle = std::thread::spawn(move || {
        rt.block_on(async {
            disk.run().await.unwrap();
        });
    });

    // Start and run the terminal UI
    let (fr_tx, fr_rx) = mpsc::channel::<FrMsg>(300);
    let mut fr = Frontend::new(fr_tx.clone(), disk_tx.clone(), config.clone());

    spawn(async move {
        fr.run(fr_rx).await.unwrap();
    });

    // If the user passed a magnet through the CLI,
    // start this torrent immediately
    if let Some(magnet) = args.magnet {
        fr_tx.send(FrMsg::NewTorrent(magnet)).await.unwrap();
    }

    handle.join().unwrap();

    Ok(())
}
