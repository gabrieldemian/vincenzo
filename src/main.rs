pub mod backend;
pub mod cli;
pub mod error;
pub mod frontend;
pub mod magnet_parser;
pub mod tcp_wire;
pub mod torrent_list;
pub mod tracker;

use backend::{Backend, BackendMessage};
use error::Error;
use magnet_parser::get_magnet;
use tokio::sync::mpsc;

#[tokio::main]
async fn main() -> Result<(), Error> {
    pretty_env_logger::init();

    let (tx, rx) = mpsc::channel::<BackendMessage>(32);

    let mut backend = Backend::new(tx.clone(), rx).await;

    let m = get_magnet(&"magnet:?xt=urn:btih:48AAC768A865798307DDD4284BE77644368DD2C7&amp;dn=Kerkour%20S.%20Black%20Hat%20Rust...Rust%20programming%20language%202022&amp;tr=udp%3A%2F%2Ftracker.coppersurfer.tk%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.openbittorrent.com%3A6969%2Fannounce&amp;tr=udp%3A%2F%2F9.rarbg.to%3A2710%2Fannounce&amp;tr=udp%3A%2F%2F9.rarbg.me%3A2780%2Fannounce&amp;tr=udp%3A%2F%2F9.rarbg.to%3A2730%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.opentrackr.org%3A1337&amp;tr=http%3A%2F%2Fp4p.arenabg.com%3A1337%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.torrent.eu.org%3A451%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.tiny-vps.com%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Fopen.stealth.si%3A80%2Fannounce").unwrap();

    tx.send(BackendMessage::AddMagnet(m)).await.unwrap();

    backend.run().await?;

    // let system = System::new();
    //
    // let fr_execution = async {
    //     Frontend::create(move |ctx| {
    //         let bk_addr = Backend::new(ctx.address().recipient());
    //
    //         Frontend::new().unwrap()
    //     });
    // };
    //
    // // spawn OS thread
    // let arbiter = Arbiter::new();
    // arbiter.spawn(fr_execution);
    //
    // system.run()?;
    Ok(())
}
