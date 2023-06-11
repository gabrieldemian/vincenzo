pub mod bitfield;
pub mod cli;
pub mod error;
pub mod frontend;
pub mod magnet_parser;
pub mod peer;
pub mod tcp_wire;
pub mod torrent;
pub mod torrent_list;
pub mod tracker;

//
//  Torrent <--> Peers --> Disk IO
//     |         |
//    \/         |
//  Tracker  <---|
//
