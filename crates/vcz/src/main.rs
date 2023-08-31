#![allow(missing_docs)]
#![allow(rustdoc::missing_doc_code_examples)]
use std::path::Path;

use tokio::{
    fs::{create_dir_all, OpenOptions},
    io::AsyncWriteExt,
};

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

use vcz_lib::FrMsg;
use vcz_ui::Frontend;

#[tokio::main]
async fn main() -> Result<(), Error> {
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

    // load the config file
    let dotfile = ProjectDirs::from("", "", "Vincenzo").ok_or(Error::HomeInvalid)?;
    let mut config_path = dotfile.config_dir().to_path_buf();

    if !config_path.exists() {
        create_dir_all(&config_path)
            .await
            .map_err(|_| Error::FolderOpenError(config_path.to_str().unwrap().to_owned()))?;
    }

    config_path.push("config.toml");

    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(&config_path)
        .await
        .expect("Error while trying to open the project config folder, please make sure this program has the right permissions.");

    let mut str = String::new();
    file.read_to_string(&mut str).await.unwrap();
    let mut config = toml::from_str::<Config>(&str);

    // the user does not have the config file, write
    // the default config
    if config.is_err() {
        let download_dir = UserDirs::new()
            .ok_or(Error::FolderNotFound(
                "home".into(),
                config_path.to_str().unwrap().to_owned(),
            ))?
            .download_dir()
            .ok_or(Error::FolderNotFound(
                "download".into(),
                config_path.to_str().unwrap().to_owned(),
            ))?
            .to_str()
            .unwrap()
            .into();

        let config_local = Config {
            download_dir,
            listen: None,
        };

        let config_str = toml::to_string(&config_local).unwrap();

        file.write_all(config_str.as_bytes()).await.unwrap();

        config = Ok(config_local);
    }

    let config = config.unwrap();

    // create a new OS thread, to be used by Disk I/O.
    // with a Tokio runtime on it.
    let args = Args::parse();
    let d = args.download_dir.unwrap_or(config.download_dir.clone());

    let (disk_tx, disk_rx) = mpsc::channel::<DiskMsg>(300);
    let mut disk = Disk::new(disk_rx, d.clone());

    if !Path::new(&d).exists() {
        return Err(Error::FolderOpenError(d));
    }

    let rt = Runtime::new().unwrap();
    let handle = std::thread::spawn(move || {
        rt.block_on(async {
            disk.run().await.unwrap();
        });
    });

    // Start and run the terminal UI
    let (fr_tx, fr_rx) = mpsc::channel::<FrMsg>(300);
    let mut fr = Frontend::new(fr_tx.clone(), disk_tx.clone(), config.clone());

    spawn(async move {
        fr.run(fr_rx).await.unwrap();
    });

    // If the user passed a magnet through the CLI,
    // start this torrent immediately
    if let Some(magnet) = args.magnet {
        fr_tx.send(FrMsg::NewTorrent(magnet)).await.unwrap();
    }

    handle.join().unwrap();

    Ok(())
}
