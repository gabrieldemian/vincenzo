use tracing::debug;
use vincenzo::error::Error;

use vcz_ui::app::App;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Error> {
    // Start and run the terminal UI
    let mut app = App::new();

    // UI is detached from the Daemon
    app.is_detached = true;

    app.run().await.unwrap();
    debug!("ui exited run");

    Ok(())
}
