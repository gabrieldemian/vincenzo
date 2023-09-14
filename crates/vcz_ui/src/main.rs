use tokio::sync::mpsc;

use tracing::debug;
use vincenzo::error::Error;

use vcz_ui::{UIMsg, UI};

#[tokio::main]
async fn main() -> Result<(), Error> {
    // Start and run the terminal UI
    let (fr_tx, fr_rx) = mpsc::channel::<UIMsg>(300);
    let mut fr = UI::new(fr_tx.clone());

    // UI is detached from the Daemon
    fr.is_detached = true;

    fr.run(fr_rx).await.unwrap();
    debug!("ui exited run");

    Ok(())
}
