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
//! we simply run the [daemon] and send messages to it.
//!
//! ```
//!    use daemon::Daemon;
//!
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
//! ```

pub mod avg;
pub mod bitfield;
pub mod cli;
pub mod config;
pub mod counter;
pub mod daemon;
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
pub mod utils;
