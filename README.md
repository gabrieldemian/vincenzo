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

Open your terminal and enter:

```bash
vcz
```

## Configuration

The binaries read a toml config file.
It is located at the default config folder of your OS.

- Linux: ~/.config/vincenzo/config.toml
- Windows: C:\\Users\\Alice\\AppData\\Roaming\\Vincenzo\\config.toml
- MacOS: /Users/Alice/Library/Application Support/Vincenzo/config.toml

### Defaults

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

- [ ] Customizable UI color scheme.

- [ ] Customizable keybindings.

- [ ] Anti-snubbing.

- [ ] Save metainfo files on disk.

- [ ] UTP

- [ ] Select files to download.

- [ ] Support streaming of videos/music on MPV.
