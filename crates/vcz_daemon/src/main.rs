use vincenzo::daemon::Daemon;

// todo: create clap config and implement cli flags

#[tokio::main]
async fn main() {
    let mut daemon = Daemon::new().await.unwrap();
    daemon.run().await.unwrap();
}
