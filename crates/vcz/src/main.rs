use magnet_url::Magnet;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::{spawn, sync::mpsc};
use tracing::Level;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::{
    Layer,
    layer::{Filter, SubscriberExt},
};
use vcz_lib::{
    config::Config,
    daemon::Daemon,
    disk::{Disk, DiskMsg, ReturnToDisk},
    error::Error,
};
use vcz_ui::{action::Action, app::App};

static LOG_ENABLED: AtomicBool = AtomicBool::new(false);

struct LogEnabledFilter;

impl<S> Filter<S> for LogEnabledFilter {
    fn enabled(
        &self,
        meta: &tracing::Metadata<'_>,
        _ctx: &tracing_subscriber::layer::Context<'_, S>,
    ) -> bool {
        if !LOG_ENABLED.load(Ordering::Relaxed) {
            return false;
        }
        meta.level() <= &Level::INFO
    }
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    let config = Arc::new(Config::load()?);
    let log_path = Config::get_log_path();
    let file_appender =
        RollingFileAppender::new(Rotation::WEEKLY, log_path, "vcz.log");
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);

    LOG_ENABLED.store(config.log, Ordering::SeqCst);

    let layer = tracing_subscriber::fmt::layer()
        .with_target(false)
        .with_writer(non_blocking)
        .with_filter(LogEnabledFilter);

    let subscriber = tracing_subscriber::registry().with(layer);

    tracing::subscriber::set_global_default(subscriber)
        .expect("setting default subscriber failed");

    tracing::info!("config: {config:?}");

    let (disk_tx, disk_rx) = mpsc::channel::<DiskMsg>(512);
    let (free_tx, free_rx) = mpsc::unbounded_channel::<ReturnToDisk>();

    let mut daemon = Daemon::new(config.clone(), disk_tx.clone(), free_tx);
    let mut disk = Disk::new(
        config.clone(),
        daemon.ctx.clone(),
        disk_tx,
        disk_rx,
        free_rx,
    );

    let disk_handle = spawn(async move { disk.run().await });
    let daemon_handle = spawn(async move { daemon.run().await });

    let (fr_tx, fr_rx) = mpsc::unbounded_channel();

    // If the user passed a magnet through the CLI,
    // start this torrent immediately
    if let Some(magnet) = &config.magnet {
        let magnet = Magnet::new(magnet)?;
        let _ = fr_tx.send(Action::NewTorrent(magnet));
    }

    // Start and run the terminal UI
    let mut app = App::new(config.clone(), fr_tx.clone());

    tokio::join!(daemon_handle, disk_handle, app.run(fr_rx)).0?
}
