use std::fs::OpenOptions;

use clap::Parser;
use tokio::{runtime::Runtime, spawn, sync::mpsc};
use tracing_subscriber::prelude::__tracing_subscriber_SubscriberExt;
use vincenzo::{
    cli::Args,
    disk::{Disk, DiskMsg},
    error::Error,
    frontend::{FrMsg, Frontend},
    torrent::Torrent,
};

#[tokio::main]
async fn main() -> Result<(), Error> {
    let console_layer = console_subscriber::spawn();
    let r = tracing_subscriber::registry();
    r.with(console_layer);
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open("log.txt")
        .expect("Failed to open log file");
    tracing_subscriber::fmt()
        .with_env_filter("tokio=trace,runtime=trace")
        .with_max_level(tracing::Level::INFO)
        .with_target(false)
        .with_writer(file)
        .compact()
        .with_file(false)
        .without_time()
        .init();
    let args = Args::parse();

    let (disk_tx, disk_rx) = mpsc::channel::<DiskMsg>(300);
    let mut disk = Disk::new(disk_rx);

    // create a new OS thread, to be used by Disk I/O.
    // with a Tokio runtime on it.
    let d = args.download_dir.clone();

    let rt = Runtime::new().unwrap();
    let handle = std::thread::spawn(move || {
        rt.block_on(async {
            disk.run(d).await.unwrap();
        });
    });

    // Start and run the terminal UI
    let (fr_tx, fr_rx) = mpsc::channel::<FrMsg>(100);
    let mut fr = Frontend::new(fr_tx.clone(), disk_tx.clone());
    spawn(async move {
        fr.run(fr_rx).await.unwrap();
    });

    // If the user passed a magnet through the CLI,
    // start this torrent immediately
    if let Some(magnet) = &args.magnet {
        let mut torrent = Torrent::new(disk_tx, magnet, &args.download_dir);

        // Add this torrent to or UI
        fr_tx
            .send(FrMsg::AddTorrent(torrent.ctx.clone()))
            .await
            .unwrap();

        spawn(async move {
            torrent.start_and_run(args.listen).await?;
            Ok::<_, Error>(())
        });
    }

    handle.join().unwrap();

    Ok(())
}
