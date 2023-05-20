use frontend::{FrontendHandle, FrontendMessage};
use models::backend::{Backend, BackendMessage};
use tokio::sync::mpsc::{self, Receiver, Sender};

pub mod frontend;
pub mod models;
pub mod torrent_list;

#[tokio::main]
async fn start_tokio(
    tx: Sender<BackendMessage>,
    rx: Receiver<BackendMessage>,
    tx_frontend: Sender<FrontendMessage>,
) {
    let mut backend = Backend::new(tx, rx);
    backend.daemon(tx_frontend).await;
}

#[tokio::main]
async fn main() -> Result<(), String> {
    pretty_env_logger::init();

    // `Network` will own these channels, but
    // `Frontend` will also have a `tx` to it.
    let (tx_backend, rx_backend) = mpsc::channel::<BackendMessage>(200);
    let tx_backend_cloned = tx_backend.clone();

    // `Network` will communicate with the frontend,
    // using this `tx`.
    let (tx_frontend, rx_app) = mpsc::channel::<FrontendMessage>(200);
    let tx_frontend_cloned = tx_frontend.clone();

    FrontendHandle::new(tx_frontend, rx_app, tx_backend);

    let daemon_handle = std::thread::spawn(move || {
        start_tokio(tx_backend_cloned, rx_backend, tx_frontend_cloned);
    });

    daemon_handle.join().unwrap();

    Ok(())
}
