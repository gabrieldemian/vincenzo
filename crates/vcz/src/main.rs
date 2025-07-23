use clap::Parser;
use tokio::{join, spawn, sync::mpsc};
use tracing::Level;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::{fmt::time::OffsetTime, FmtSubscriber};
use vincenzo::{
    args::Args, config::CONFIG, daemon::Daemon, disk::Disk, error::Error,
};

use vcz_ui::{action::Action, app::App};

#[tokio::main(flavor = "current_thread")]
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
        .with_max_level(Level::DEBUG)
        .with_writer(non_blocking)
        .with_timer(OffsetTime::new(
            time::UtcOffset::current_local_offset()
                .unwrap_or(time::UtcOffset::UTC),
            time::format_description::parse(
                "[year]-[month]-[day] [hour]:[minute]:[second]",
            )
            .unwrap(),
        ))
        .with_ansi(false)
        .finish();

    tracing::subscriber::set_global_default(subscriber)
        .expect("setting default subscriber failed");

    tracing::info!("config: {:?}", *CONFIG);

    if CONFIG.max_global_peers == 0 || CONFIG.max_torrent_peers == 0 {
        return Err(Error::ConfigError(
            "max_global_peers or max_torrent_peers cannot be zero".into(),
        ));
    }

    if CONFIG.max_global_peers < CONFIG.max_torrent_peers {
        return Err(Error::ConfigError(
            "max_global_peers cannot be less than max_torrent_peers".into(),
        ));
    }

    let mut disk = Disk::new(CONFIG.download_dir.clone());
    let disk_tx = disk.tx.clone();

    let mut daemon = Daemon::new(disk_tx);

    spawn(async move {
        let _ = disk.run().await;
    });

    let (fr_tx, fr_rx) = mpsc::unbounded_channel();

    // Start and run the terminal UI
    let mut fr = App::new(fr_tx.clone());

    let args = Args::parse();

    // If the user passed a magnet through the CLI,
    // start this torrent immediately
    if let Some(magnet) = args.magnet {
        let _ = fr_tx.send(Action::NewTorrent(magnet));
    }

    let (v1, v2) = join!(daemon.run(), fr.run(fr_rx));
    v1?;
    v2.unwrap();

    Ok(())
}
