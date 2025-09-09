use magnet_url::Magnet;
use tokio::{spawn, sync::mpsc};
use tracing::Level;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::FmtSubscriber;
use vincenzo::{
    config::CONFIG,
    daemon::Daemon,
    disk::{Disk, DiskMsg, ReturnToDisk},
    error::Error,
};

use vcz_ui::{action::Action, app::App};

#[tokio::main]
async fn main() -> Result<(), Error> {
    let tmp = std::env::temp_dir();

    let time = std::time::SystemTime::now();
    let timestamp =
        time.duration_since(std::time::UNIX_EPOCH).unwrap().as_millis();

    let file_appender = RollingFileAppender::new(
        Rotation::NEVER,
        tmp,
        format!("vcz-{timestamp}.log"),
    );
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);

    let subscriber = FmtSubscriber::builder()
        .without_time()
        .with_target(false)
        .with_max_level(Level::INFO)
        .with_writer(non_blocking)
        .finish();

    tracing::subscriber::set_global_default(subscriber)
        .expect("setting default subscriber failed");

    tracing::info!("config: {:?}", *CONFIG);

    let (disk_tx, disk_rx) = mpsc::channel::<DiskMsg>(512);
    let (free_tx, free_rx) = mpsc::unbounded_channel::<ReturnToDisk>();

    let mut daemon = Daemon::new(disk_tx.clone(), free_tx.clone());
    let mut disk = Disk::new(daemon.ctx.clone(), disk_tx, disk_rx, free_rx);

    let disk_handle = spawn(async move { disk.run().await });
    let daemon_handle = spawn(async move { daemon.run().await });

    let (fr_tx, fr_rx) = mpsc::unbounded_channel();

    // Start and run the terminal UI
    let mut fr = App::new(fr_tx.clone());

    // If the user passed a magnet through the CLI,
    // start this torrent immediately
    if let Some(magnet) = &CONFIG.magnet {
        let magnet = Magnet::new(magnet)?;
        let _ = fr_tx.send(Action::NewTorrent(magnet));
    }

    // let fr_handle = spawn(async move { fr.run(fr_rx).await });

    disk_handle.await??;
    daemon_handle.await??;
    fr.run(fr_rx).await.unwrap();

    Ok(())
}
