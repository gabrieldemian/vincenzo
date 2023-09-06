#![allow(missing_docs)]
#![allow(rustdoc::missing_doc_code_examples)]
mod error;
use tokio::fs::{create_dir_all, OpenOptions};

use clap::Parser;
use directories::{ProjectDirs, UserDirs};
use tokio::{io::AsyncReadExt, runtime::Runtime, spawn, sync::mpsc};
use tracing_subscriber::prelude::__tracing_subscriber_SubscriberExt;

use vcz_lib::{
    cli::Args,
    config::Config,
    disk::{Disk, DiskMsg},
    error::Error,
};

// todo: create Daemon struct
//       daemon should have all torrents txs
//       daemon listen to tcp on event loop
//       daemon can send Draw to UI
//       daemon can receive Quit from UI
//
//       daemon initialization \/
//       daemon can create and spawn torrents
//       daemon can spawn disk

fn main() {
    let console_layer = console_subscriber::spawn();
    let r = tracing_subscriber::registry();
    r.with(console_layer);

    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("log.txt")
        .expect("Failed to open log file");

    tracing_subscriber::fmt()
        .with_env_filter("tokio=trace,runtime=trace")
        .with_max_level(tracing::Level::INFO)
        .with_target(false)
        .with_writer(file)
        .compact()
        .with_file(false)
        .without_time()
        .init();
}
