# Overview
This is the binary of the daemon. A daemon is simply the "backend" that is responsible
for adding torrents and writing/reading data on disk, communicating with the UI,
and writing logs.

The daemon listen on a TCP address that can be set on the configuration file,
or through the CLI flag `--listen`. The default address is: `127.0.0.1:3030`.

You can communicate with the Daemon in 2 days:
- CLI flags
- TCP messages

The documentation can be found on the library crate.

You need to have a download_dir on the config file or through the CLI,
having none of these will error.

```bash
vczd -d "/home/user/Downloads" -m "<insert magnet link here>"
```

Adding torrents after the daemon is running:
```bash
vczd -m "<insert second magnet link here>"
```
