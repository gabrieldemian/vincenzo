![image](tape.gif)

# Vincenzo

Vincenzo is a library for building programs powered by the BitTorrent protocol.

This crate offers the building blocks of the entire protocol, so users can build anything with it: libraries, binaries, or even new UIs.

## Features
- BitTorrent V1 protocol
- Multi-platform
- Support for magnet links
- Async I/O with tokio
- UDP connections with trackers
- Daemon detached from UI

# Example

To download a torrent, we can simply run the daemon and send messages to it.

```
    use vincenzo::daemon::Daemon;
    use vincenzo::daemon::DaemonMsg;
    use vincenzo::magnet::Magnet;
    use tokio::spawn;
    use tokio::sync::oneshot;

    #[tokio::main]
    async fn main() {
        let download_dir = "/home/gabriel/Downloads".to_string();

        let mut daemon = Daemon::new(download_dir);
        let tx = daemon.ctx.tx.clone();

        spawn(async move {
            daemon.run().await.unwrap();
        });

        let magnet = Magnet::new("magnet:?xt=urn:btih:ab6ad7ff24b5ed3a61352a1f1a7811a8c3cc6dde&amp;dn=archlinux-2023.09.01-x86_64.iso").unwrap();

        // identifier of the torrent
        let info_hash = magnet.parse_xt();

        tx.send(DaemonMsg::NewTorrent(magnet)).await.unwrap();

        // get information about the torrent download
        let (otx, orx) = oneshot::channel();

        tx.send(DaemonMsg::RequestTorrentState(info_hash, otx)).await.unwrap();
        let torrent_state = orx.await.unwrap();

        // TorrentState {
        //     name: "torrent name",
        //     download_rate: 999999,
        //     ...
        // }
    }
 ```

## Supported BEPs
- [BEP 0003](http://www.bittorrent.org/beps/bep_0003.html) - The BitTorrent Protocol Specification
- [BEP 0009](http://www.bittorrent.org/beps/bep_0009.html) - Extension for Peers to Send Metadata Files
- [BEP 0010](http://www.bittorrent.org/beps/bep_0010.html) - Extension Protocol
- [BEP 0015](http://www.bittorrent.org/beps/bep_0015.html) - UDP Tracker Protocol
- [BEP 0023](http://www.bittorrent.org/beps/bep_0023.html) - Tracker Returns Compact Peer Lists
