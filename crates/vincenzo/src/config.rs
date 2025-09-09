//! Configuration file and CLI flags.
//!
//! We have 3 components for the configuration, in order of priority :
//!
//! Environment --overrides--> CLI Flags --overrides--> File

use std::{net::SocketAddr, path::PathBuf, sync::LazyLock};

use clap::Parser;
use serde::Deserialize;

use crate::{daemon::Daemon, error::Error};

#[derive(Deserialize, Debug, Clone, Parser, Default)]
#[clap(name = "Vincenzo", author = "Gabriel Lombardo")]
#[command( author, version, about = None, long_about = None,)]
pub struct Config {
    /// Where to store files of torrents. Defaults the download dir of the
    /// user's home.
    #[clap(long)]
    #[serde(default)]
    pub download_dir: Option<PathBuf>,

    /// Where to store .torrent files. Defaults to
    /// `~/.config/vincenzo/torrents`.
    #[clap(long)]
    #[serde(default)]
    pub metadata_dir: Option<PathBuf>,

    /// Where the daemon listens for connections. Defaults to `0.0.0.0:0`.
    #[clap(long)]
    #[serde(default)]
    pub daemon_addr: Option<SocketAddr>,

    /// Port of the client, defaults to 51413
    #[clap(long)]
    #[serde(default)]
    pub local_peer_port: Option<u16>,

    /// Max numbers of global TCP connections, defaults to 500.
    #[clap(long)]
    #[serde(default)]
    pub max_global_peers: Option<u32>,

    /// Max number of TCP connections for each torrent, defaults to 50, and is
    /// capped by `max_global_peers`.
    #[clap(long)]
    #[serde(default)]
    pub max_torrent_peers: Option<u32>,

    /// If the client will use an ipv6 socket to connect to other peers.
    /// Defaults to false.
    #[clap(long)]
    #[serde(default)]
    pub is_ipv6: Option<bool>,

    /// If the daemon should quit after all downloads are complete. Defaults to
    /// false.
    #[clap(long)]
    #[serde(default)]
    pub quit_after_complete: Option<bool>,

    /// ID that the peer sends to trackers, defaults to a random number.
    #[clap(skip)]
    #[serde(skip, default)]
    pub key: Option<u32>,

    // -------------------------
    // Command fields (CLI only)
    // -------------------------
    /// Add magnet url to the daemon.
    #[clap(short, long)]
    #[serde(skip)]
    pub magnet: Option<String>,

    /// Print the stats of all torrents.
    #[clap(short, long)]
    #[serde(skip)]
    pub stats: bool,

    /// Pause the torrent with the given info hash.
    #[clap(short, long)]
    #[serde(skip)]
    pub pause: Option<String>,

    /// Terminate the process of the daemon.
    #[clap(short, long)]
    #[serde(skip)]
    pub quit: bool,
}

pub static CONFIG: LazyLock<ResolvedConfig> = LazyLock::new(|| {
    if cfg!(test) { Config::load_test() } else { Config::load().unwrap() }
});

impl Config {
    /// Try to load the configuration. Environmental variables have priviledge
    /// over values from the configuration file. If both are not set, it will
    /// try to guess the default values using $HOME.
    pub fn load() -> Result<ResolvedConfig, Error> {
        let cli_config = Self::parse();
        let file_config = Self::load_toml_config()?;
        Ok(Self::merge(file_config, cli_config).resolve())
    }

    // #[cfg(test)]
    pub(crate) fn load_test() -> ResolvedConfig {
        ResolvedConfig {
            download_dir: "/tmp/downloads".into(),
            metadata_dir: "/tmp/vincenzo".into(),
            daemon_addr: Daemon::DEFAULT_LISTENER,
            local_peer_port: 51413,
            max_global_peers: 500,
            max_torrent_peers: 50,
            is_ipv6: false,
            quit_after_complete: false,
            key: 123,
            magnet: None,
            quit: false,
            stats: false,
            pause: None,
        }
    }

    // ~/.config/vincenzo
    fn get_config_folder() -> PathBuf {
        let mut config_file = dirs::config_dir().expect(
            "Could not get the user's config directory. Have you configured \
             $XDG_CONFIG_DIR ?",
        );
        config_file.push("vincenzo");
        config_file
    }

    fn load_toml_config() -> Result<Self, Error> {
        let mut config_file = Self::get_config_folder();
        config_file.push("config");

        config::Config::builder()
            .add_source(
                config::File::with_name(config_file.to_str().unwrap())
                    .required(false), // .format(config::FileFormat::Toml),
            )
            .add_source(config::Environment::default())
            .build()
            .map_err(Error::FromConfigError)?
            .try_deserialize()
            .map_err(Error::FromConfigError)
    }

    fn merge(file_config: Self, cli_config: Self) -> Self {
        let s = Self {
            download_dir: cli_config.download_dir.or(file_config.download_dir),
            metadata_dir: cli_config.metadata_dir.or(file_config.metadata_dir),
            daemon_addr: cli_config.daemon_addr.or(file_config.daemon_addr),
            local_peer_port: cli_config
                .local_peer_port
                .or(file_config.local_peer_port),
            max_global_peers: cli_config
                .max_global_peers
                .or(file_config.max_global_peers),
            max_torrent_peers: cli_config
                .max_torrent_peers
                .or(file_config.max_torrent_peers),
            is_ipv6: cli_config.is_ipv6.or(file_config.is_ipv6),
            quit_after_complete: cli_config
                .quit_after_complete
                .or(file_config.quit_after_complete),
            key: cli_config.key.or(file_config.key),

            // Command fields come only from CLI
            magnet: cli_config.magnet,
            stats: cli_config.stats,
            pause: cli_config.pause,
            quit: cli_config.quit,
        };
        if s.max_global_peers == Some(0) || s.max_torrent_peers == Some(0) {
            panic!("max_global_peers or max_torrent_peers cannot be zero");
        }

        if s.max_global_peers < s.max_torrent_peers {
            panic!("max_global_peers cannot be less than max_torrent_peers");
        }
        s
    }

    pub fn resolve(self) -> ResolvedConfig {
        let mut metadata_dir = Self::get_config_folder();
        metadata_dir.push("torrents");

        let download_dir = dirs::download_dir()
            .expect("Could not read your download directory.");

        ResolvedConfig {
            download_dir: self.download_dir.unwrap_or(download_dir),
            metadata_dir: self.metadata_dir.unwrap_or(metadata_dir),
            daemon_addr: self.daemon_addr.unwrap_or(Daemon::DEFAULT_LISTENER),
            local_peer_port: self.local_peer_port.unwrap_or(51413),
            max_global_peers: self.max_global_peers.unwrap_or(500),
            max_torrent_peers: self.max_torrent_peers.unwrap_or(50),
            is_ipv6: self.is_ipv6.unwrap_or(false),
            quit_after_complete: self.quit_after_complete.unwrap_or(false),
            key: self.key.unwrap_or(rand::random()),

            // Command fields come only from CLI
            magnet: self.magnet,
            stats: self.stats,
            pause: self.pause,
            quit: self.quit,
        }
    }
}

#[derive(Debug)]
pub struct ResolvedConfig {
    pub download_dir: PathBuf,
    pub metadata_dir: PathBuf,
    pub daemon_addr: SocketAddr,
    pub local_peer_port: u16,
    pub max_global_peers: u32,
    pub max_torrent_peers: u32,
    pub is_ipv6: bool,
    pub quit_after_complete: bool,
    pub key: u32,

    // -------------------------
    // Command fields (CLI only)
    // -------------------------
    pub magnet: Option<String>,
    pub stats: bool,
    pub pause: Option<String>,
    pub quit: bool,
}

pub static CONFIG_BINCODE: LazyLock<
    bincode::config::Configuration<
        bincode::config::BigEndian,
        bincode::config::Varint,
    >,
> = LazyLock::new(|| {
    bincode::config::standard().with_big_endian().with_variable_int_encoding()
});

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn environment() {
        unsafe {
            std::env::set_var("DOWNLOAD_DIR", "/new/download");
        }
        let config = Config::load().unwrap();
        assert_eq!(config.download_dir.to_str().unwrap(), "/new/download");
    }

    #[test]
    fn override_config() {
        // test that CLI values override file values
        let file_config = Config {
            download_dir: Some("/file/path".into()),
            metadata_dir: Some("/file/path".into()),
            daemon_addr: Some("127.0.0.1:8080".parse().unwrap()),
            local_peer_port: Some(8080),
            max_global_peers: Some(100),
            max_torrent_peers: Some(10),
            is_ipv6: Some(false),
            quit_after_complete: Some(false),
            key: Some(123),
            magnet: None,
            stats: false,
            pause: None,
            quit: false,
        };

        let cli_config = Config {
            download_dir: Some("/cli/path".into()),
            metadata_dir: Some("/cli/path".into()),
            daemon_addr: Some("127.0.0.1:9090".parse().unwrap()),
            local_peer_port: Some(9090),
            max_global_peers: None,
            max_torrent_peers: None,
            is_ipv6: Some(true),
            quit_after_complete: Some(true),
            key: None,
            magnet: Some("magnet:test".to_string()),
            stats: true,
            pause: Some("pause_hash".to_string()),
            quit: true,
        };

        let merged = Config::merge(file_config.clone(), cli_config.clone());

        // CLI values should override file values
        assert_eq!(merged.download_dir, cli_config.download_dir);
        assert_eq!(merged.daemon_addr, cli_config.daemon_addr);
        assert_eq!(merged.local_peer_port, cli_config.local_peer_port);
        assert_eq!(merged.is_ipv6, cli_config.is_ipv6);
        assert_eq!(merged.quit_after_complete, cli_config.quit_after_complete);

        // these should fall back to file values since CLI didn't provide them
        assert_eq!(merged.max_global_peers, file_config.max_global_peers);
        assert_eq!(merged.max_torrent_peers, file_config.max_torrent_peers);
        assert_eq!(merged.key, file_config.key);

        // command fields should come from CLI only
        assert_eq!(merged.magnet, cli_config.magnet);
        assert!(merged.stats);

        assert_eq!(merged.pause, cli_config.pause);
        assert!(merged.quit);
    }
}
