use clap::Parser;
use tokio::{runtime::Runtime, spawn, sync::mpsc};
use tracing::debug;
use tracing::Level;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::fmt::time::OffsetTime;
use tracing_subscriber::FmtSubscriber;
use vincenzo::daemon::Daemon;
use vincenzo::error::Error;
use vincenzo::{config::Config, daemon::Args};

use vcz_ui::{UIMsg, UI};

#[tokio::main]
async fn main() -> Result<(), Error> {
    let tmp = std::env::temp_dir();
    let time = std::time::SystemTime::now();
    let timestamp = time
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis();

    let file_appender =
        RollingFileAppender::new(Rotation::NEVER, tmp, format!("vcz-{timestamp}.log"));
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);

    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::DEBUG)
        .with_writer(non_blocking)
        .with_timer(OffsetTime::new(
            time::UtcOffset::current_local_offset().unwrap_or(time::UtcOffset::UTC),
            time::format_description::parse("[year]-[month]-[day] [hour]:[minute]:[second]")
                .unwrap(),
        ))
        .with_ansi(false)
        .finish();

    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");

    let args = Args::parse();
    let config = Config::load().await.unwrap();

    let download_dir = args.download_dir.unwrap_or(config.download_dir.clone());
    let daemon_addr = args.daemon_addr.unwrap_or(
        config
            .daemon_addr
            .unwrap_or("127.0.0.1:3030".parse().unwrap()),
    );

    let mut daemon = Daemon::new(download_dir);
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
