#![allow(missing_docs)]

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

#[derive(Debug, Clone)]
pub enum FrMsg {
    NewTorrent(String),
    Draw([u8; 20], TorrentState),
    TogglePause([u8; 20]),
    Quit,
}

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
