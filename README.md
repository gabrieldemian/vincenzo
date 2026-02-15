# Vincenzo

Vincenzo is a BitTorrent client with vim-like keybindings and a terminal based UI.

![image](tape.gif)

## About

Vincenzo aims to be a modern, fast, minimalistic\*, and good-looking BitTorrent
client.

\*Minimalistic here means that the UI is not bloated, configuration is done
through the configuration file, no ads, and no telemetry.

The official UI runs on the terminal with vim-like keybindings.

Vcz offers 3 binaries:

- [vcz](crates/vcz) - Both UI and daemon together.
- [vcz-daemon](crates/vcz-daemon) - Daemon.
- [vcz-ui](crates/vcz-ui) - UI (connects to daemon remotely).

> [!WARNING]
> Experimental software, not production ready yet.
>
> I develop on rust nightly.
> I have only tested on my x86 Linux machine, but _probably_ works in other platforms.
>
> The protocol is fully implemented with good performance and with many nice
> extensions that you would expect, however, some things are missing and sometimes
> there are crashes. Security features are not present yet.

## Features

- Multi-platform.
- Fast downloads.
- Daemon detached from the UI. Remote control by TCP messages or UI binary in
  another machine. (note that this is not secured by authentication yet).

## How to use

Right now, you have to download the repo and compile it from scratch, remember
to use the `--release`.

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
  -d, --download-dir
                    dir to write torrent files
  --metadata-dir    dir to store .torrent files
  --daemon-addr     where daemon listens for connections
  --local-peer-port port that the client connect to other peers
  --max-global-peers
                    max global TCP connections
  --max-torrent-peers
                    max peers in each torrent, capped by `max_global_peers
  --is-ipv6         if the client addr is ipv6
  --log             if the program writes logs to disk
  -q, --quit-after-complete
                    make daemon quit after all downloads are completed
  -k, --key         key that peer sends to trackers, defaults to random
  -m, --magnet      add magnet url to the daemon
  --stats           print the stats of all torrents
  --pause           pause this torrent
  --quit            terminate the process of the daemon
  -h, --help        display usage information
```

### Default config.toml

```toml
download_dir = "$XDG_DOWNLOAD_DIR"
daemon_addr = "0.0.0.0:51411"
max_global_peers = 500
max_torrent_peers = 50
local_peer_port = 51413
is_ipv6 = false
log = false
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
