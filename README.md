# Vicenzo
!⚠️ Work in progress. !⚠️ <br />
A minimalistic, terminal based, BitTorrent client with vim keybindings. Torrents can be downloaded using the CLI or UI. Still a work in progress, however, it is already usable.

## Features
- Multi-platform <br />
- Magnet links support <br />
- Terminal based with vim keybindings <br />
- UDP connections with trackers, TCP connections with peers <br />
- Multithreaded. One OS thread specific for I/O. <br />

## How to use
An example on how to download a torrent using the CLI. Please use the "--help" flag to read the descriptions of the CLI flags.

```bash
cargo run -- -d "/tmp/btr" -m "<insert magnet link here>" -q
```

Or

```bash
vcz -d "/tmp/btr" -m "<insert magnet link here>" -q
```

## Supported BEPs
- [BEP 0003](http://www.bittorrent.org/beps/bep_0003.html) - The BitTorrent Protocol Specification
- [BEP 0009](http://www.bittorrent.org/beps/bep_0009.html) - Extension for Peers to Send Metadata Files
- [BEP 0010](http://www.bittorrent.org/beps/bep_0010.html) - Extension Protocol
- [BEP 0015](http://www.bittorrent.org/beps/bep_0015.html) - UDP Tracker Protocol
- [BEP 0023](http://www.bittorrent.org/beps/bep_0023.html) - Tracker Returns Compact Peer Lists

## Roadmap
In order of priority. <br />
⏳ - Initial version of UI <br />
⏳ - Use a buffered I/O strategy to reduce the number of writes on disk. <br />
⏳ - Upgrade the download algorithm to use the maximum bitrate. <br />
⏳ - Resume torrent download. <br />
⏳ - Change piece download strategy. <br />
⏳ - Select files to download. <br />
⏳ - Support streaming of videos/music on MPV. <br />

## Tests
This program is well-tested and I'm always improving the tests.
