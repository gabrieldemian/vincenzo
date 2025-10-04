# Vincenzo

Vincenzo is a BitTorrent client with vim-like keybindings and a terminal based UI.

![image](tape.gif)

## About

Vincenzo aims to be a modern, fast, minimalistic\*, and good-looking BitTorrent client.

\*Minimalistic here means that the UI is not bloated, configuration is done
through the configuration file, no ads, and no telemetry.

The official UI runs on the terminal with vim-like keybindings.

3 binaries and 1 library:

- [vcz](crates/vcz) - Main binary with both UI and daemon together.
- [vcz-ui](crates/vcz-ui) - UI binary (connects to daemon remotely).
- [vczd](crates/vcz-daemon) - Daemon binary.
- [vcz-lib](crates/vcz-lib) - Library.

> [!WARNING]
> Experimental software moving towards a stable release.
>
> I develop on rust nightly and there is no minimum supported version.
> I have only tested on my x86 Linux machine, but _probably_ works in other platforms.
>
> The protocol is fully implemented with good performance and with many nice
> extensions that you would expect, however, some things are missing and sometimes
> there are crashes. Security features and authentication for remote
> control is not yet implemented.

## Features

- Multi-platform.
- Fast downloads.
- Daemon detached from the UI. Remote control by TCP messages or UI binary in
  another machine. (note that this is not secured by authentication yet).

## How to use

Right now, you have to download the repo and compile it from scratch, remember
to use the `--release` flag of cargo.

```bash
$ git clone git@github.com:gabrieldemian/vincenzo.git
$ cd vincenzo
$ cargo build --release
$ cd ./target/release
./vcz
```

## Configuration

We have 3 sources of configuration, in order of lowest priority to the highest:
config file -> CLI flags -> ENV variables.

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
          Max number of TCP connections for each torrent, defaults to 50, and is
          capped by `max_global_peers`

      --is-ipv6 <IS_IPV6>
          If the client will use an ipv6 socket to connect to other peers.
          Defaults to false [possible values: true, false]

      --quit-after-complete <QUIT_AFTER_COMPLETE>
          If the daemon should quit after all downloads are complete. Defaults
          to false [possible values: true, false]

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
max_global_peers = 500
max_torrent_peers = 50
local_peer_port = 51413
is_ipv6 = false
```

## Supported BEPs

- [BEP 0003](http://www.bittorrent.org/beps/bep_0003.html)
The BitTorrent Protocol Specification.

- [BEP 0009](http://www.bittorrent.org/beps/bep_0009.html)
Extension for Peers to Send Metadata Files.

- [BEP 0010](http://www.bittorrent.org/beps/bep_0010.html)
Extension Protocol.

- [BEP 0015](http://www.bittorrent.org/beps/bep_0015.html)
UDP Tracker Protocol.

- [BEP 0023](http://www.bittorrent.org/beps/bep_0023.html)
Tracker Returns Compact Peer Lists.

## Roadmap

|  #  | Step                                                      | Status |
| :-: | --------------------------------------------------------- | :----: |
|  1  | Base core protocol and algorithms: endgame, unchoke, etc. |   ✅   |
|  2  | Quality of life extensions: magnet, UDP trackers, etc.    |   ✅   |
|  3  | Perf: cache, multi-tracker, fast IO, zero-copy, etc.      |   ✅   |
|  4  | Saving metainfo files on disk after downloading it        |   ✅   |
|  5  | Select files to download before download starts           |   ❌   |
|  6  | Streaming of videos                                       |   ❌   |
|  7  | µTP, UDP hole punch, and fancy features                   |   ⚠️   |
| ... | Others                                                    |   ❌   |
