#![feature(unwrap_infallible)]
#![feature(ip_as_octets)]
#![feature(trait_alias)]
#![feature(never_type)]

pub static VERSION: &str = "0.0.1";

/// Version number part of the PeerID.
/// 0001
pub static VERSION_PROT: &[u8; 4] = b"0001";

pub const DAEMON_MSG_BOUND: usize = 128;
pub const DISK_MSG_BOUND: usize = 512;
pub const PEER_BR_MSG_BOUND: usize = 1024;
pub const PEER_MSG_BOUND: usize = 32;
pub const TORRENT_MSG_BOUND: usize = 256;
pub const TRACKER_MSG_BOUND: usize = 32;

pub mod bitfield;
pub mod config;
pub mod counter;
pub mod daemon;
pub mod daemon_wire;
pub mod dirs;
pub mod disk;
pub mod error;
pub mod extensions;
pub mod magnet;
pub mod metainfo;
pub mod peer;
pub mod torrent;
pub mod tracker;
pub mod utils;
