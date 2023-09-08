#![allow(missing_docs)]
#![allow(rustdoc::missing_doc_code_examples)]
mod error;
use vcz_daemon::Daemon;

#[tokio::main]
async fn main() {
    let mut daemon = Daemon::new().await.unwrap();
    daemon.run().await.unwrap();
}
