//! Config file
use std::{net::SocketAddr, sync::LazyLock};

use serde::{Deserialize, Serialize};

use crate::error::Error;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Config {
    pub download_dir: String,
    pub daemon_addr: SocketAddr,
    pub quit_after_complete: bool,
}

static CONFIG: LazyLock<config::Config> = LazyLock::new(|| {
    let home = std::env::var("HOME").expect(
        "The $HOME env var is not set, therefore the program cant use default \
         values, you should set them manually on the configuration file or \
         through CLI flags. Use --help.",
    );

    let download_dir = std::env::var("XDG_DOWNLOAD_DIR")
        .unwrap_or(format!("{home}/Downloads"));

    // config.toml, the .toml part is omitted.
    // right now this default guess only works in linux and macos.
    let config_file = std::env::var("XDG_CONFIG_HOME")
        .map(|v| format!("{v}/vincenzo/config"))
        .unwrap_or(format!("{home}/.config/vincenzo/config"));

    config::Config::builder()
        .add_source(config::File::with_name(&config_file).required(false))
        .add_source(config::Environment::default())
        .set_default("download_dir", download_dir)
        .unwrap()
        .set_default("daemon_addr", "127.0.0.1:3030")
        .unwrap()
        .set_default("quit_after_complete", false)
        .unwrap()
        .build()
        .unwrap()
});

impl Config {
    /// Try to load the configuration. Environmental variables have priviledge
    /// over values from the configuration file. If both are not set, it will
    /// try to guess the default values using $HOME.
    pub fn load() -> Result<Self, Error> {
        CONFIG
            .clone()
            .try_deserialize::<Self>()
            .map_err(|_| Error::ConfigDeserializeError)
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    #[test]
    fn override_config() {
        std::env::set_var("DOWNLOAD_DIR", "/new/download");

        let parsed = Config::load().unwrap();

        assert_eq!(parsed.download_dir, "/new/download".to_owned());
    }
}
