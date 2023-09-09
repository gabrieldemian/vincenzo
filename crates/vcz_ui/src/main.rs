use tokio::sync::mpsc;

use tracing::debug;
use vcz_lib::error::Error;

use vcz_lib::FrMsg;
use vcz_ui::Frontend;

#[tokio::main]
async fn main() -> Result<(), Error> {
    // Start and run the terminal UI
    let (fr_tx, fr_rx) = mpsc::channel::<FrMsg>(300);
    let mut fr = Frontend::new(fr_tx.clone());

    // UI is detached from the Daemon
    fr.is_detached = true;

    fr.run(fr_rx).await.unwrap();
    debug!("ui exited run");

    Ok(())
}
