use std::fs::OpenOptions;

use bittorrent_rust::{
    cli::Args,
    disk::{Disk, DiskMsg},
    error::Error,
    torrent::Torrent,
};
use clap::Parser;
use tokio::{runtime::Runtime, spawn, sync::mpsc};
use tracing_subscriber::prelude::__tracing_subscriber_SubscriberExt;

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
    // let m = get_magnet("magnet:?xt=urn:btih:48aac768a865798307ddd4284be77644368dd2c7&dn=Kerkour%20S.%20Black%20Hat%20Rust...Rust%20programming%20language%202022&tr=udp%3A%2F%2Ftracker.coppersurfer.tk%3A6969%2Fannounce&tr=udp%3A%2F%2F9.rarbg.to%3A2920%2Fannounce&tr=udp%3A%2F%2Ftracker.opentrackr.org%3A1337&tr=udp%3A%2F%2Ftracker.internetwarriors.net%3A1337%2Fannounce&tr=udp%3A%2F%2Ftracker.leechers-paradise.org%3A6969%2Fannounce&tr=udp%3A%2F%2Ftracker.pirateparty.gr%3A6969%2Fannounce&tr=udp%3A%2F%2Ftracker.cyberia.is%3A6969%2Fannounce").unwrap();
    // cargo run -- -d "/tmp/btr" -m "magnet:?xt=urn:btih:48aac768a865798307ddd4284be77644368dd2c7&dn=Kerkour%20S.%20Black%20Hat%20Rust...Rust%20programming%20language%202022&tr=udp%3A%2F%2Ftracker.coppersurfer.tk%3A6969%2Fannounce&tr=udp%3A%2F%2F9.rarbg.to%3A2920%2Fannounce&tr=udp%3A%2F%2Ftracker.opentrackr.org%3A1337&tr=udp%3A%2F%2Ftracker.internetwarriors.net%3A1337%2Fannounce&tr=udp%3A%2F%2Ftracker.leechers-paradise.org%3A6969%2Fannounce&tr=udp%3A%2F%2Ftracker.pirateparty.gr%3A6969%2Fannounce&tr=udp%3A%2F%2Ftracker.cyberia.is%3A6969%2Fannounce" -q -l 0.0.0.0:55123
    // music
    // magnet:?xt=urn:btih:9281EF9099967ED8413E87589EFD38F9B9E484B0&amp;dn=The%20Doors%20%20(Complete%20Studio%20Discography%20-%20MP3%20%40%20320kbps)&amp;tr=udp%3A%2F%2Ftracker.coppersurfer.tk%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.openbittorrent.com%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.bittor.pw%3A1337%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.opentrackr.org%3A1337&amp;tr=udp%3A%2F%2Fbt.xxx-tracker.com%3A2710%2Fannounce&amp;tr=udp%3A%2F%2Fpublic.popcorn-tracker.org%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Feddie4.nl%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.torrent.eu.org%3A451%2Fannounce&amp;tr=udp%3A%2F%2Fp4p.arenabg.com%3A1337%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.tiny-vps.com%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Fopen.stealth.si%3A80%2Fannounce
    // cargo run -- -d "/tmp/btr" -m "magnet:?xt=urn:btih:9281EF9099967ED8413E87589EFD38F9B9E484B0&amp;dn=The%20Doors%20%20(Complete%20Studio%20Discography%20-%20MP3%20%40%20320kbps)&amp;tr=udp%3A%2F%2Ftracker.coppersurfer.tk%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.openbittorrent.com%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.bittor.pw%3A1337%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.opentrackr.org%3A1337&amp;tr=udp%3A%2F%2Fbt.xxx-tracker.com%3A2710%2Fannounce&amp;tr=udp%3A%2F%2Fpublic.popcorn-tracker.org%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Feddie4.nl%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.torrent.eu.org%3A451%2Fannounce&amp;tr=udp%3A%2F%2Fp4p.arenabg.com%3A1337%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.tiny-vps.com%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Fopen.stealth.si%3A80%2Fannounce" -l 0.0.0.0:55123 -q
    // book
    // cargo run -- -d "/tmp/btr" -m "magnet:?xt=urn:btih:48aac768a865798307ddd4284be77644368dd2c7&dn=Kerkour%20S.%20Black%20Hat%20Rust...Rust%20programming%20language%202022&tr=udp%3A%2F%2Ftracker.coppersurfer.tk%3A6969%2Fannounce&tr=udp%3A%2F%2F9.rarbg.to%3A2920%2Fannounce&tr=udp%3A%2F%2Ftracker.opentrackr.org%3A1337&tr=udp%3A%2F%2Ftracker.internetwarriors.net%3A1337%2Fannounce&tr=udp%3A%2F%2Ftracker.leechers-paradise.org%3A6969%2Fannounce&tr=udp%3A%2F%2Ftracker.pirateparty.gr%3A6969%2Fannounce&tr=udp%3A%2F%2Ftracker.cyberia.is%3A6969%2Fannounce" -l 0.0.0.0:55123 -q
    let args = Args::parse();

    let (disk_tx, disk_rx) = mpsc::channel::<DiskMsg>(300);
    let mut disk = Disk::new(disk_rx);

    // todo: start UI here
    // ...

    // create a new OS thread, to be used by Disk I/O.
    // with a Tokio runtime on it.
    let d = args.download_dir.clone();

    let rt = Runtime::new().unwrap();
    let handle = std::thread::spawn(move || {
        rt.block_on(async {
            disk.run(d).await.unwrap();
        });
    });

    // If the user passed a magnet through the CLI,
    // start this torrent immediately
    if let Some(magnet) = &args.magnet {
        let mut torrent = Torrent::new(disk_tx, magnet, &args.download_dir);

        spawn(async move {
            torrent.start_and_run(args.listen).await?;
            Ok::<_, Error>(())
        });
    }

    handle.join().unwrap();

    Ok(())
}
