use clap::Parser;
use vincenzo::{
    config::Config,
    daemon::{Args, Daemon},
};
mod args;

#[tokio::main]
async fn main() {
    let args = Args::parse();
    let config = Config::load().await.unwrap();

    let download_dir = args.download_dir.unwrap_or(config.download_dir.clone());
    let mut listen = config.daemon_addr;

    if args.daemon_addr.is_some() {
        listen = args.daemon_addr;
    }

    let mut daemon = Daemon::new(download_dir).await;

    if let Some(listen) = listen {
        daemon.config.listen = listen;
    }

    let _tx = daemon.ctx.tx.clone();

    daemon.run().await.unwrap();
}
