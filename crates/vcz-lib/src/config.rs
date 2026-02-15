//! Configuration file and CLI flags.
//!
//! We have 2 components for the configuration, in order of priority :
//!
//! CLI Flags --overrides--> File

use crate::{daemon::Daemon, error::Error};
use clap::Parser;
use hashbrown::HashMap;
use serde::Deserialize;
use std::{net::SocketAddr, path::PathBuf};

#[derive(Deserialize, Debug, Clone, Parser, Default)]
#[clap(name = "Vincenzo", author = "Gabriel Lombardo")]
#[command(author, version, about = None, long_about = None)]
pub struct Config {
    /// Where to store files of torrents. Defaults to the download dir of the
    /// user.
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

    /// Max number of global TCP connections, defaults to 500.
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

    /// If the program will write logs to disk.
    /// Defaults to false.
    #[clap(long)]
    #[serde(default)]
    pub log: Option<bool>,

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

impl Config {
    /// Load the configuration file from disk and CLI, resolve and merge them.
    pub fn load() -> Result<ResolvedConfig, Error> {
        let cli_config = Self::parse();
        let file_config = Self::from_file()?;
        Ok(Config::merge(file_config, cli_config).resolve())
    }

    #[cfg(feature = "debug")]
    pub fn load_test() -> ResolvedConfig {
        let test_files_dir =
            Self::find_workspace_root().unwrap().join("test-files");

        ResolvedConfig {
            config_dir: "".into(),
            download_dir: test_files_dir.clone(),
            metadata_dir: test_files_dir,
            daemon_addr: "0.0.0.0:0".parse().unwrap(),
            local_peer_port: rand::random_range(49152..65535),
            max_global_peers: 500,
            max_torrent_peers: 50,
            is_ipv6: false,
            log: false,
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

    pub fn get_log_path() -> PathBuf {
        let mut p = dirs::data_local_dir().expect(
            "Could not get the user's local directory. Have you configured \
             $XDG_DATA_HOME ?",
        );
        p.push("vincenzo");
        p
    }

    /// Get the workspace root path
    fn find_workspace_root() -> Option<PathBuf> {
        let mut cur = std::env::current_dir().ok()?;
        // cur.parent()?.parent().map(|v| v.into())
        let mut i = 0;

        loop {
            i += 1;
            if i >= 5 {
                break;
            }
            let cargo_toml = cur.join("Cargo.toml");

            if cargo_toml.exists()
                && let Ok(content) = std::fs::read_to_string(&cargo_toml)
                && content.contains("[workspace]")
            {
                return Some(cur);
            }

            if !cur.pop() {
                break;
            }
        }

        None
    }

    fn merge(file_config: Config, cli_config: Self) -> Self {
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
            log: cli_config.log.or(file_config.log),
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

    fn resolve(self) -> ResolvedConfig {
        let mut metadata_dir = Self::get_config_folder();
        metadata_dir.push("torrents");

        let download_dir = dirs::download_dir()
            .expect("Could not read your download directory.");

        let mut config_dir = Self::get_config_folder();
        config_dir.push("config");

        ResolvedConfig {
            config_dir,
            download_dir: self.download_dir.unwrap_or(download_dir),
            metadata_dir: self.metadata_dir.unwrap_or(metadata_dir),
            daemon_addr: self.daemon_addr.unwrap_or(Daemon::DEFAULT_LISTENER),
            local_peer_port: self.local_peer_port.unwrap_or(51413),
            max_global_peers: self.max_global_peers.unwrap_or(500),
            max_torrent_peers: self.max_torrent_peers.unwrap_or(50),
            is_ipv6: self.is_ipv6.unwrap_or(false),
            log: self.log.unwrap_or(false),
            quit_after_complete: self.quit_after_complete.unwrap_or(false),
            key: self.key.unwrap_or(rand::random()),

            // Command fields come only from CLI
            magnet: self.magnet,
            stats: self.stats,
            pause: self.pause,
            quit: self.quit,
        }
    }

    fn from_file() -> Result<Self, Error> {
        let mut config_file = Config::get_config_folder();
        config_file.push("config");
        let content = std::fs::read_to_string(config_file).unwrap();
        Self::from_str(&content)
    }

    fn parse_bool(s: &str) -> Result<bool, Error> {
        s.parse::<bool>().map_err(|e| e.into())
    }

    fn parse_u16(s: &str) -> Result<u16, Error> {
        s.parse::<u16>().map_err(|e| e.into())
    }

    fn parse_u32(s: &str) -> Result<u32, Error> {
        s.parse::<u32>().map_err(|e| e.into())
    }

    fn parse_quoted_string(raw: &str) -> Result<String, Error> {
        let trimmed = raw.trim();
        if !trimmed.starts_with('"') || !trimmed.ends_with('"') {
            return Err(Error::ParseStrError);
        }
        Ok(trimmed[1..trimmed.len() - 1].to_string())
    }

    fn from_str(input: &str) -> Result<Self, Error> {
        let mut map = HashMap::new();

        for line in input.lines() {
            let line = line.trim();

            // look for an '='
            let Some(eq_pos) = line.find('=') else { continue };

            let key = line[..eq_pos].trim();
            let value_part = line[eq_pos + 1..].trim();

            if key.is_empty() {
                return Err(Error::ParseStrError);
            }

            // remove anything after an #
            let value = match value_part.find('#') {
                Some(idx) => {
                    // ensure the '#' is not inside quotes (very basic check)
                    let before_hash = &value_part[..idx];
                    if before_hash.contains('"') {
                        // we have a quote before '#', so the '#' might be part
                        // of a string. For simplicity,
                        // we treat the whole value_part as the value.
                        value_part
                    } else {
                        before_hash.trim()
                    }
                }
                None => value_part,
            };
            map.insert(key.to_string(), value.to_string());
        }

        let is_ipv6 = map.get("is_ipv6").map(|v| Self::parse_bool(v).into_ok());
        let log = map.get("log").map(|v| Self::parse_bool(v).into_ok());
        let quit_after_complete = map
            .get("quit_after_complete")
            .map(|v| Self::parse_bool(v).into_ok());
        let local_peer_port =
            map.get("local_peer_port").map(|v| Self::parse_u16(v).into_ok());
        let max_global_peers =
            map.get("max_global_peers").map(|v| Self::parse_u32(v).into_ok());
        let key = map.get("key").map(|v| Self::parse_u32(v).into_ok());
        let max_torrent_peers =
            map.get("max_torrent_peers").map(|v| Self::parse_u32(v).into_ok());
        let daemon_addr = map
            .get("daemon_addr")
            .map(|v| Self::parse_quoted_string(v).into_ok());
        let daemon_addr = if let Some(addr) = daemon_addr {
            Some(addr.parse().map_err(|_| Error::ParseStrError)?)
        } else {
            None
        };
        let metadata_dir = map
            .get("metadata_dir")
            .map(|v| PathBuf::from(Self::parse_quoted_string(v).into_ok()));
        let download_dir = map
            .get("download_dir")
            .map(|v| PathBuf::from(Self::parse_quoted_string(v).into_ok()));

        Ok(Self {
            daemon_addr,
            download_dir,
            is_ipv6,
            key,
            local_peer_port,
            log,
            max_global_peers,
            max_torrent_peers,
            metadata_dir,
            quit_after_complete,
            ..Default::default()
        })
    }
}

#[derive(Debug)]
pub struct ResolvedConfig {
    pub download_dir: PathBuf,
    pub metadata_dir: PathBuf,
    pub config_dir: PathBuf,
    pub daemon_addr: SocketAddr,
    pub local_peer_port: u16,
    pub max_global_peers: u32,
    pub max_torrent_peers: u32,
    pub is_ipv6: bool,
    pub log: bool,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge() {
        // test that CLI values override file values
        let file_config = Config {
            download_dir: Some("/file/path".into()),
            metadata_dir: Some("/file/path".into()),
            daemon_addr: Some("127.0.0.1:8080".parse().unwrap()),
            local_peer_port: Some(8080),
            max_global_peers: Some(100),
            max_torrent_peers: Some(10),
            is_ipv6: Some(false),
            log: Some(false),
            quit_after_complete: Some(false),
            key: Some(123),
            ..Default::default()
        };

        let cli_config = Config {
            download_dir: Some("/cli/path".into()),
            metadata_dir: Some("/cli/path".into()),
            daemon_addr: Some("127.0.0.1:9090".parse().unwrap()),
            local_peer_port: Some(9090),
            max_global_peers: None,
            max_torrent_peers: None,
            is_ipv6: Some(true),
            log: Some(true),
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
        assert_eq!(merged.log, cli_config.log);

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

    #[test]
    fn decode() {
        let toml = r#"
            is_ipv6 = true
            local_peer_port = 8080
            daemon_addr = "127.0.0.1:9000"
            metadata_dir = "/tmp/metadata_dir"
        "#;
        let config = Config::from_str(toml).unwrap();
        assert_eq!(config.is_ipv6, Some(true));
        assert_eq!(config.local_peer_port, Some(8080));
        assert_eq!(config.daemon_addr, Some("127.0.0.1:9000".parse().unwrap()));
        assert_eq!(
            config.metadata_dir,
            Some(PathBuf::from("/tmp/metadata_dir"))
        );
    }

    #[test]
    fn decode_potential_errors() {
        let toml = r#"
            [notsupported]
            # this will be skiped
            is_ipv6 = true #heh
            # wtf
            wtf_is_a_kilometer = true
        "#;
        let config = Config::from_str(toml).unwrap();
        assert_eq!(config.is_ipv6, Some(true));
        assert_eq!(config.local_peer_port, None);
        assert_eq!(config.daemon_addr, None);
        assert_eq!(config.metadata_dir, None);
    }
}
