//! A library for working with the BitTorrent protocol V1.
//!
//! This is the library created for Vincenzo, a BitTorrent client. It uses this
//! library to create both the daemon and ui binaries.
//!
//! This crate contains building blocks for developing software with this protocol
//! in a high-level manner. Basically, BitTorrent is a decentralized, peer-to-peer
//! file sharing protocol, this empowers you to build many powerful things.
//!
//! A few ideas:
//!
//! * A new UI for the Vincenzo daemon
//! * A program that synchronizes files between multiple servers
//!
//! This library has the tools for both the UI and the Daemon parts of a client.
//!
//! # Example
//!
//! This is how you can download torrents using just the daemon,
//! we simply run the [daemon][daemon] and send messages to it.
//!
//! ```
//!    let mut daemon = Daemon::new().await.unwrap();
//!    let tx = daemon.ctx.tx.clone();
//!
//!    spawn(async move {
//!        daemon.run().await.unwrap();
//!    });
//!
//!    tx.send(DaemonMsg::NewTorrent("magnet:........")).await.unwrap();
//!
//!    // the download directory can be read from the CLi,
//!    // or on the configuration file, that's all we need.
//!    // You can also chose the address that the daemon will run on,
//!    // and a few more options.
//! ``

use speedy::{Readable, Writable};
use torrent::{Stats, TorrentStatus};
pub mod avg;
pub mod bitfield;
pub mod cli;
pub mod config;
pub mod counter;
pub mod daemon_wire;
pub mod disk;
pub mod error;
pub mod extension;
pub mod magnet_parser;
pub mod metainfo;
pub mod peer;
pub mod tcp_wire;
pub mod torrent;
pub mod tracker;

/// transform bytes into a human readable format.
pub fn to_human_readable(mut n: f64) -> String {
    let units = ["B", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB", "ZiB", "YiB"];
    let delimiter = 1024_f64;
    if n < delimiter {
        return format!("{} {}", n, "B");
    }
    let mut u: i32 = 0;
    let r = 10_f64;
    while (n * r).round() / r >= delimiter && u < (units.len() as i32) - 1 {
        n /= delimiter;
        u += 1;
    }
    format!("{:.2} {}", n, units[u as usize])
}

/// Messages used by [`UI`] for internal communication.
#[derive(Debug, Clone)]
pub enum UIMsg {
    NewTorrent(String),
    Draw(TorrentState),
    TogglePause([u8; 20]),
    Quit,
}

/// Messages used by the [`Daemon`] for internal communication.
/// All of these local messages have an equivalent remote message
/// on [`DaemonMsg`].
#[derive(Debug, Clone)]
pub enum DaemonMsg {
    NewTorrent(String),
    TorrentState(TorrentState),
    // TogglePause([u8; 20]),
    Quit,
}

/// State of a particular [`Torrent`], used by the [`UI`] to present data.
#[derive(Debug, Clone, Default, PartialEq, Readable, Writable)]
pub struct TorrentState {
    pub name: String,
    pub stats: Stats,
    pub status: TorrentStatus,
    pub downloaded: u64,
    pub download_rate: u64,
    pub uploaded: u64,
    pub size: u64,
    pub info_hash: [u8; 20],
}

#[cfg(test)]
mod tests {
    use crate::to_human_readable;

    #[test]
    pub fn readable_size() {
        let n = 495353_f64;
        assert_eq!(to_human_readable(n), "483.74 KiB");

        let n = 30_178_876_f64;
        assert_eq!(to_human_readable(n), "28.78 MiB");

        let n = 2093903856_f64;
        assert_eq!(to_human_readable(n), "1.95 GiB");
    }
}
