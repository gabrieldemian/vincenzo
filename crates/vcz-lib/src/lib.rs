#![feature(macro_metavar_expr)]
#![feature(ip_as_octets)]
#![feature(trait_alias)]

pub static VERSION: &str = "0.0.1";

/// Version number part of the PeerID.
/// 0.00.00
pub static VERSION_PROT: &[u8; 5] = b"00001";

pub mod bitfield;
pub mod config;
pub mod counter;
pub mod daemon;
pub mod daemon_wire;
pub mod disk;
pub mod error;
pub mod extensions;
pub mod magnet;
pub mod metainfo;
pub mod peer;
pub mod torrent;
pub mod tracker;
pub mod utils;
