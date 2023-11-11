//! A library for working with the BitTorrent protocol V1.
//!
//! This is the library created for Vincenzo, a BitTorrent client. It uses this
//! library to create both the daemon and ui binaries.
//!
//! This crate contains building blocks for developing software using this
//! protocol in a high-level manner.
//!
//! A few ideas that benefit from a distributed and decentralized protocol:
//!
//! * A program that synchronizes files between multiple peers.
//! * A fully encrypted chat client with files.
//!
//! # Example
//!
//! This is how you can download torrents using just the daemon,
//! we simply run the [daemon] and send messages to it.
//!
//! ```
//!    use vincenzo::daemon::Daemon;
//!    use vincenzo::daemon::DaemonMsg;
//!    use vincenzo::magnet::Magnet;
//!    use tokio::spawn;
//!    use tokio::sync::oneshot;
//!
//!    #[tokio::main]
//!    async fn main() {
//!        let download_dir = "/home/gabriel/Downloads".to_string();
//!
//!        let mut daemon = Daemon::new(download_dir);
//!        let tx = daemon.ctx.tx.clone();
//!
//!        spawn(async move {
//!            daemon.run().await.unwrap();
//!        });
//!
//!        let magnet = Magnet::new("magnet:?xt=urn:btih:ab6ad7ff24b5ed3a61352a1f1a7811a8c3cc6dde&amp;dn=archlinux-2023.09.01-x86_64.iso").unwrap();
//!
//!        // identifier of the torrent
//!        let info_hash = magnet.parse_xt();
//!
//!        tx.send(DaemonMsg::NewTorrent(magnet)).await.unwrap();
//!
//!        // get information about the torrent download
//!        let (otx, orx) = oneshot::channel();
//!
//!        tx.send(DaemonMsg::RequestTorrentState(info_hash, otx)).await.unwrap();
//!        let torrent_state = orx.await.unwrap();
//!
//!        // TorrentState {
//!        //     name: "torrent name",
//!        //     download_rate: 999999,
//!        //     ...
//!        // }
//!    }
//! ```

pub mod avg;
pub mod bitfield;
pub mod config;
pub mod counter;
pub mod daemon;
pub mod daemon_wire;
pub mod disk;
pub mod error;
pub mod extension;
pub mod magnet;
pub mod metainfo;
pub mod peer;
pub mod tcp_wire;
pub mod torrent;
pub mod tracker;
pub mod utils;
