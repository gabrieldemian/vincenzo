# Overview
This is the crate of the daemon. A daemon is simply the "backend" that is responsible
for adding torrents and writing/reading data on disk.

The daemon is a web server that is running on a port, this port can be defined in the
configuration file or passed through a CLI flag. `--listen`.

A daemon will also write logs in case of a panic.

The daemon can send information about torrents to anyone that requests it.

Starting the daemon and optionally adding a torrent. You need to have a download_dir on
the config file or through the CLI. Having none of those will error.
```bash
vczd -d "/home/user/Downloads" -m "<insert magnet link here>"
```

Adding torrents after the daemon is running:
```bash
vczd -m "<insert second magnet link here>"
```
