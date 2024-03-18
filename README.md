# Vincenzo
Vincenzo is a BitTorrent client with vim-like keybindings and a terminal based UI.

[![Latest Version](https://img.shields.io/crates/v/vincenzo.svg)](https://crates.io/crates/vincenzo) ![MIT](https://img.shields.io/badge/license-MIT-blue.svg)

![image](tape.gif)

## Introduction
Vincenzo aims to be a fast, lightweight, and multi-platform client.

Another goal is for users to be able to use the [library](crates/vincenzo) to create any other kind of software, that is powered by the BitTorrent protocol.

The official UI binary is very niched, targeting a very specific type of user: someone who loves using the terminal, and vim keybindings. Altough users could create other UIs using the library.

Vincenzo offers 3 binaries and 1 library:

- [vcz](crates/vcz) - Main binary with both UI and daemon.
- [vcz_ui](crates/vcz_ui) - UI binary.
- [vczd](crates/vcz_daemon) - Daemon binary.
- [vincenzo](crates/vincenzo) - Library

## Features
- Multi-platform.
- Multithreaded. One OS thread specific for I/O.
- Async I/O with tokio.
- Communication with daemon using CLI flags, TCP messages or remotely via UI binary.
- Detached daemon from the UI.
- Support for magnet links.

## How to use
Downloading a torrent using the main binary (the flags are optional and could be omitted in favour of the configuration file).

```bash
vcz -d "/tmp/download_dir" -m "<magnet link here>" -q
```

## Configuration
The binaries read a toml config file.
It is located at the default config folder of your OS.
- Linux:   ~/.config/vincenzo/config.toml
- Windows: C:\Users\Alice\AppData\Roaming\Vincenzo\config.toml
- MacOS:   /Users/Alice/Library/Application Support/Vincenzo/config.toml

### Default config file:
```toml
download_dir = "/home/alice/Downloads"
# default
daemon_addr = "127.0.0.1:3030"
```

## Daemon and UI binaries
Users can control the Daemon by using CLI flags that work as messages.

Let's say on one terminal you initiate the daemon: `vczd`. Or spawn as a background process so you can do everything on one terminal: `vczd &`.

And you open a second terminal to send messages to the daemon, add a torrent: `vczd -m "magnet:..."` and then print the stats to stdout `vczd --stats`.

You can also run the UI binary (maybe remotely from another machine) to control the Daemon: `vcz_ui --daemon-addr 127.0.0.1:3030`.

<details>
<summary>CLI flags of Daemon</summary>

```
Usage: vczd [OPTIONS]

Options:
      --daemon-addr <DAEMON_ADDR>    The Daemon will accept TCP connections on this address
  -d, --download-dir <DOWNLOAD_DIR>  The directory in which torrents will be downloaded
  -m, --magnet <MAGNET>              Download a torrent using it's magnet link, wrapped in quotes
  -q, --quit-after-complete          If the program should quit after all torrents are fully downloaded
  -s, --stats                        Print all torrent status on stdout
  -h, --help                         Print help
  -V, --version                      Print version
  ```
</details>

## Supported BEPs
- [BEP 0003](http://www.bittorrent.org/beps/bep_0003.html) - The BitTorrent Protocol Specification
- [BEP 0009](http://www.bittorrent.org/beps/bep_0009.html) - Extension for Peers to Send Metadata Files
- [BEP 0010](http://www.bittorrent.org/beps/bep_0010.html) - Extension Protocol
- [BEP 0015](http://www.bittorrent.org/beps/bep_0015.html) - UDP Tracker Protocol
- [BEP 0023](http://www.bittorrent.org/beps/bep_0023.html) - Tracker Returns Compact Peer Lists

## NixOS Install Instructions

<details>
<summary>Using nix profile</summary>

To install Vincenzo using `nix profile`, you can use the following commands:

```bash
# Install Vincenzo using nix profile
nix profile install github:gabrieldemian/vincenzo
```

</details>

<details>
<summary>Using Nix Flake and Home-Manager</summary>

If you're using `Nix Flakes` and `Home-Manager`, you can add Vincenzo to your `configuration.nix` file:

```nix
# flake.nix

{
  inputs.vincenzo.url = "github:gabrieldemian/vincenzo";
  # ...

  outputs = {nixpkgs, ...} @ inputs: {
    nixosConfigurations.HOSTNAME = nixpkgs.lib.nixosSystem {
      specialArgs = { inherit inputs; }; # this is the important part
      modules = [
        ./configuration.nix
      ];
    };
  }
}

```

```nix
# configuration.nix

{config, inputs, pkgs, ...}: {
  programs.vincenzo = {
    enable = true;
    package = inputs.vincenzo.packages.${pkgs.system}.default;
    download_dir = config.xdg.userDirs.downloads
  };
}

```

</details>

## Roadmap
- [x] Initial version of UI. <br />
- [x] Download pipelining. <br />
- [x] Endgame mode. <br />
- [x] Pause and resume torrents. <br />
- [x] Separate main binary into 3 binaries and 1 library. <br />
- [x] Cache bytes to reduce the number of writes on disk. <br />
- [x] Change piece selection strategy. <br />
- [ ] Choking algorithm. <br />
- [ ] Anti-snubbing. <br />
- [ ] Resume torrent download from a file. <br />
- [ ] Select files to download. <br />
- [ ] Support streaming of videos/music on MPV. <br />

## Donations
I'm working on this alone, if you enjoy my work, please consider a donation [here](https://www.glombardo.dev/sponsor).

