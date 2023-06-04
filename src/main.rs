pub mod cli;
pub mod error;
pub mod frontend;
pub mod magnet_parser;
pub mod tcp_wire;
pub mod torrent;
pub mod torrent_list;
pub mod tracker;

use error::Error;
use magnet_parser::get_magnet;
use tokio::{spawn, sync::mpsc};
use torrent::{Torrent, TorrentMsg};

#[tokio::main]
async fn main() -> Result<(), Error> {
    pretty_env_logger::init();

    let (tx, rx) = mpsc::channel::<TorrentMsg>(32);

    let mut torrent = Torrent::new(tx.clone(), rx).await;

    // let m = get_magnet(&"magnet:?xt=urn:btih:48AAC768A865798307DDD4284BE77644368DD2C7&amp;dn=Kerkour%20S.%20Black%20Hat%20Rust...Rust%20programming%20language%202022&amp;tr=udp%3A%2F%2Ftracker.coppersurfer.tk%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.openbittorrent.com%3A6969%2Fannounce&amp;tr=udp%3A%2F%2F9.rarbg.to%3A2710%2Fannounce&amp;tr=udp%3A%2F%2F9.rarbg.me%3A2780%2Fannounce&amp;tr=udp%3A%2F%2F9.rarbg.to%3A2730%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.opentrackr.org%3A1337&amp;tr=http%3A%2F%2Fp4p.arenabg.com%3A1337%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.torrent.eu.org%3A451%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.tiny-vps.com%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Fopen.stealth.si%3A80%2Fannounce").unwrap();
    let m = get_magnet(&"magnet:?xt=urn:btih:4E64AAAF48D922DBD93F8B9E4ACAA78C99BC1F40&amp;dn=MICROSOFT%20Office%20PRO%20Plus%202016%20v16.0.4266.1003%20RTM%20%2B%20Activator&amp;tr=udp%3A%2F%2Ftracker.coppersurfer.tk%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.openbittorrent.com%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.bittor.pw%3A1337%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.opentrackr.org%3A1337&amp;tr=udp%3A%2F%2Fbt.xxx-tracker.com%3A2710%2Fannounce&amp;tr=udp%3A%2F%2Fpublic.popcorn-tracker.org%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Feddie4.nl%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.torrent.eu.org%3A451%2Fannounce&amp;tr=udp%3A%2F%2Fp4p.arenabg.com%3A1337%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.tiny-vps.com%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Fopen.stealth.si%3A80%2Fannounce").unwrap();
    tx.send(TorrentMsg::AddMagnet(m)).await.unwrap();

    torrent.run().await.unwrap();

    Ok(())
}
