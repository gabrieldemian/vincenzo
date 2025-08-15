use tokio::sync::mpsc;
use vcz_ui::{app::App, error::Error};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Error> {
    let (fr_tx, fr_rx) = mpsc::unbounded_channel();

    // Start and run the terminal UI
    let mut app = App::new(fr_tx.clone());

    // UI is detached from the Daemon
    app.is_detached = true;

    app.run(fr_rx).await?;

    Ok(())
}
