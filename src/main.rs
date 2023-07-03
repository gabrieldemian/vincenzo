use std::fs::OpenOptions;

use bittorrent_rust::{
    disk::{Disk, DiskMsg},
    error::Error,
    magnet_parser::get_magnet,
    torrent::{Torrent, TorrentMsg},
};
use tokio::{runtime::Runtime, spawn, sync::mpsc};
use tracing_subscriber::prelude::__tracing_subscriber_SubscriberExt;

#[tokio::main]
async fn main() -> Result<(), Error> {
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open("log.txt")
        .expect("Failed to open log file");

    let console_layer = console_subscriber::spawn();
    let r = tracing_subscriber::registry();
    r.with(console_layer);

    tracing_subscriber::fmt()
        .with_env_filter("tokio=trace,runtime=trace")
        .with_max_level(tracing::Level::INFO)
        .with_target(false)
        .with_writer(file)
        .compact()
        .with_file(false)
        .without_time()
        .init();

    // console_subscriber::init();

    let m = get_magnet("magnet:?xt=urn:btih:48aac768a865798307ddd4284be77644368dd2c7&dn=Kerkour%20S.%20Black%20Hat%20Rust...Rust%20programming%20language%202022&tr=udp%3A%2F%2Ftracker.coppersurfer.tk%3A6969%2Fannounce&tr=udp%3A%2F%2F9.rarbg.to%3A2920%2Fannounce&tr=udp%3A%2F%2Ftracker.opentrackr.org%3A1337&tr=udp%3A%2F%2Ftracker.internetwarriors.net%3A1337%2Fannounce&tr=udp%3A%2F%2Ftracker.leechers-paradise.org%3A6969%2Fannounce&tr=udp%3A%2F%2Ftracker.pirateparty.gr%3A6969%2Fannounce&tr=udp%3A%2F%2Ftracker.cyberia.is%3A6969%2Fannounce").unwrap();
    // let m = get_magnet("magnet:?xt=urn:btih:4E64AAAF48D922DBD93F8B9E4ACAA78C99BC1F40&amp;dn=MICROSOFT%20Office%20PRO%20Plus%202016%20v16.0.4266.1003%20RTM%20%2B%20Activator&amp;tr=udp%3A%2F%2Ftracker.coppersurfer.tk%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.openbittorrent.com%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.bittor.pw%3A1337%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.opentrackr.org%3A1337&amp;tr=udp%3A%2F%2Fbt.xxx-tracker.com%3A2710%2Fannounce&amp;tr=udp%3A%2F%2Fpublic.popcorn-tracker.org%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Feddie4.nl%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.torrent.eu.org%3A451%2Fannounce&amp;tr=udp%3A%2F%2Fp4p.arenabg.com%3A1337%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.tiny-vps.com%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Fopen.stealth.si%3A80%2Fannounce").unwrap();
    // let m = get_magnet("magnet:?xt=urn:btih:6e8537c9160e80042f0bc5a880ea8bf9144683ff&dn=archlinux-2023.06.01-x86_64.iso").unwrap();

    let (torrent_tx, torrent_rx) = mpsc::channel::<TorrentMsg>(300);
    let (disk_tx, disk_rx) = mpsc::channel::<DiskMsg>(300);

    let mut torrent = Torrent::new(torrent_tx, disk_tx, torrent_rx, m).await;
    let torrent_ctx = torrent.ctx.clone();
    let mut disk = Disk::new(disk_rx, torrent_ctx);

    spawn(async move {
        let peers = torrent.start().await?;
        torrent.spawn_peers_tasks(peers).await?;
        torrent.run().await?;
        Ok::<_, Error>(())
    });

    // create a new OS thread, to be used by Disk I/O.
    // with a Tokio runtime on it.
    let rt = Runtime::new().unwrap();
    std::thread::spawn(move || {
        rt.block_on(async {
            disk.run().await.unwrap();
        });
    })
    .join()
    .unwrap();

    Ok(())
}
