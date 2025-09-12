# Vincenzo

Vincenzo is a BitTorrent client with vim-like keybindings and a terminal based UI.

[![Latest Version](https://img.shields.io/crates/v/vincenzo.svg)](https://crates.io/crates/vincenzo) ![MIT](https://img.shields.io/badge/license-MIT-blue.svg)

![image](tape.gif)

## Introduction

Vincenzo aims to be a modern, fast, and good-looking BitTorrent client.

The official UI runs on the terminal with vim-like keybindings.

3 binaries and 1 library:

- [vcz](crates/vcz) - Main binary with both UI and daemon together.
- [vcz_ui](crates/vcz_ui) - UI binary (requires daemon).
- [vczd](crates/vcz_daemon) - Daemon binary.
- [vincenzo](crates/vincenzo) - Library.

⚠️ Experimental and unstable, expect bugs and problems, stability is always increasing until the first stable version.

⚠️ I use Rust nightly to develop, not guaranteed to work on other versions.

⚠️ I use Linux x86, never tested on other platforms.

## Features

- Multi-platform, altough I have only tested on Linux x86.
- Async I/O with Tokio.
- TUI with Ratatui.
- Detached daemon from the UI. Communication by TCP making it possible to create other UI's.

## How to use

The easiest way is to download or compile the "vcz" binary which contains both UI and daemon, and run it on the terminal:

```bash
vcz
```

You can also have the daemon running and the UI in another machine, but that requires changing the configuration.

## Configuration

We have 3 sources of configuration, in order of lowest priority to the highest: config file -> CLI flags -> ENV variables.

The config file is located at the default config folder of your OS.

- Linux: ~/.config/vincenzo/config.toml
- Windows: C:\\Users\\Alice\\AppData\\Roaming\\Vincenzo\\config.toml
- MacOS: /Users/Alice/Library/Application Support/Vincenzo/config.toml

For the CLI flags, use --help.

### CLI flags

```text
$vcz --help

Usage: vczd [OPTIONS]

Options:
      --download-dir <DOWNLOAD_DIR>
          Where to store files of torrents. Defaults to the download dir of the user.
      --metadata-dir <METADATA_DIR>
          Where to store .torrent files. Defaults to `~/.config/vincenzo/torrents`
      --daemon-addr <DAEMON_ADDR>
          Where the daemon listens for connections. Defaults to `0.0.0.0:0`
      --local-peer-port <LOCAL_PEER_PORT>
          Port of the client, defaults to 51413
      --max-global-peers <MAX_GLOBAL_PEERS>
          Max number of global TCP connections, defaults to 500
      --max-torrent-peers <MAX_TORRENT_PEERS>
          Max number of TCP connections for each torrent, defaults to 50, and is capped by `max_global_peers`
      --is-ipv6 <IS_IPV6>
          If the client will use an ipv6 socket to connect to other peers. Defaults to false [possible values: true, false]
      --quit-after-complete <QUIT_AFTER_COMPLETE>
          If the daemon should quit after all downloads are complete. Defaults to false [possible values: true, false]
  -m, --magnet <MAGNET>
          Add magnet url to the daemon
  -s, --stats
          Print the stats of all torrents
  -p, --pause <PAUSE>
          Pause the torrent with the given info hash
  -q, --quit
          Terminate the process of the daemon
  -h, --help
          Print help
  -V, --version
          Print version
```

### Default config.toml

```toml
download_dir = "$XDG_DOWNLOAD_DIR"
daemon_addr = "0.0.0.0:51411"
max_global_peers = 200
max_torrent_peers = 50
local_peer_port = 51413
is_ipv6 = false
```

## Supported BEPs

- [BEP 0003](http://www.bittorrent.org/beps/bep_0003.html) - The BitTorrent Protocol Specification
- [BEP 0009](http://www.bittorrent.org/beps/bep_0009.html) - Extension for Peers to Send Metadata Files
- [BEP 0010](http://www.bittorrent.org/beps/bep_0010.html) - Extension Protocol
- [BEP 0015](http://www.bittorrent.org/beps/bep_0015.html) - UDP Tracker Protocol
- [BEP 0023](http://www.bittorrent.org/beps/bep_0023.html) - Tracker Returns Compact Peer Lists

## Roadmap

Not necessarily in this order:

- [x] Base core protocol.

- [x] Base core algorithms: optimistic unchoke, unchoke, endgame mode, etc.

- [x] TUI.

- [x] Pause and resume torrents.

- [x] Cache writes.

- [x] Save metainfo files on disk.

- [x] Multi-tracker.

- [ ] Customizable UI color scheme.

- [ ] Customizable keybindings.

- [ ] Anti-snubbing.

- [ ] UTP

- [ ] Select files to download.

- [ ] Support streaming of videos/music on MPV.
