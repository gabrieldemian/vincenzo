//! Config file
use std::{net::SocketAddr, sync::LazyLock};

use serde::{Deserialize, Serialize};

use crate::{daemon::Daemon, error::Error};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Config {
    /// Root folder to place the torrents files
    pub download_dir: String,

    /// Daemon will listen to this addr for UI connections or CLI commands,
    pub daemon_addr: SocketAddr,

    /// Port where the client will listen for connections.
    pub local_peer_port: u16,

    /// Maximum number of peers that the client can have.
    pub max_global_peers: u32,

    /// Maximum number of peers per torrent.
    pub max_torrent_peers: u32,

    /// If the local peer is running on ipv6.
    pub is_ipv6: bool,

    /// Quit the daemon after fully downloading all torrents.
    pub quit_after_complete: bool,

    pub key: u32,
}

pub static CONFIG: LazyLock<Config> = LazyLock::new(|| Config::get().unwrap());

impl Config {
    /// Try to load the configuration. Environmental variables have priviledge
    /// over values from the configuration file. If both are not set, it will
    /// try to guess the default values using $HOME.
    pub fn load() -> Result<Self, Error> {
        Self::get()
    }

    fn get() -> Result<Config, Error> {
        let home = std::env::var("HOME").expect(
            "The $HOME env var is not set, therefore the program cant use \
             default values, you should set them manually on the \
             configuration file or through CLI flags. Use --help.",
        );

        let download_dir = std::env::var("XDG_DOWNLOAD_DIR")
            .unwrap_or(format!("{home}/Downloads"));

        // config.toml, the .toml part is omitted.
        // right now this default guess only works in linux and macos.
        let config_file = std::env::var("XDG_CONFIG_HOME")
            .map(|v| format!("{v}/vincenzo/config"))
            .unwrap_or(format!("{home}/.config/vincenzo/config"));

        let key: u32 = rand::random();

        config::Config::builder()
            .add_source(config::File::with_name(&config_file).required(false))
            .add_source(config::Environment::default())
            .set_default("download_dir", download_dir)
            .unwrap()
            .set_default("daemon_addr", Daemon::DEFAULT_LISTENER.to_string())
            .unwrap()
            .set_default("max_global_peers", 200)
            .unwrap()
            .set_default("local_peer_port", 51413)
            .unwrap()
            .set_default("max_torrent_peers", 50)
            .unwrap()
            .set_default("quit_after_complete", false)
            .unwrap()
            .set_default("is_ipv6", false)
            .unwrap()
            .set_default("key", key)
            .unwrap()
            .build()?
            .try_deserialize::<Config>()
            .map_err(Error::FromConfigError)
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    #[test]
    fn override_config() {
        std::env::set_var("DOWNLOAD_DIR", "/new/download");

        assert_eq!(CONFIG.download_dir, "/new/download".to_owned());
    }
}
