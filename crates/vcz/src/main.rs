use clap::Parser;
use tokio::join;
use tracing::Level;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::{fmt::time::OffsetTime, FmtSubscriber};
use vincenzo::{args::Args, daemon::Daemon};

use vcz_ui::{action::Action, app::App};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
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

    let mut daemon = Daemon::new();

    // Start and run the terminal UI
    let mut fr = App::new();
    let fr_tx = fr.tx.clone();

    let args = Args::parse();

    // If the user passed a magnet through the CLI,
    // start this torrent immediately
    if let Some(magnet) = args.magnet {
        fr_tx.send(Action::NewTorrent(magnet)).unwrap();
    }

    let (v1, v2) = join!(daemon.run(), fr.run());
    v1?;
    v2?;

    Ok(())
}
