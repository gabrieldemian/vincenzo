use std::{fs::create_dir_all, net::SocketAddr, time::Duration};

use bitvec::{bitvec, prelude::Msb0};
use futures::{SinkExt, StreamExt};
use rand::{distributions::Alphanumeric, Rng};
use tokio::{
    fs::OpenOptions,
    io::AsyncWriteExt,
    net::{TcpListener, TcpStream},
    select, spawn,
    sync::mpsc,
    time::interval,
};
use tracing::debug;
use vincenzo::{
    daemon::DaemonMsg,
    disk::{Disk, DiskMsg},
    magnet::Magnet,
    metainfo::Info,
    peer::{Direction, Peer, PeerMsg},
    tcp_wire::{messages::Message, Block, BlockInfo},
    torrent::{Torrent, TorrentMsg},
    tracker::Tracker,
};

// Test that a peer will re-request block_infos after timeout,
// this test will spawn a tracker-less torrent and simulate 2 peers
// communicating with each other, a seeder and a leecher.
//
// The leecher will request one block, the seeder will not answer,
// and then the leecher must send the request again.
//
// todo: this is extremely verbose to setup, maybe testing 2
// different daemons running would be easier.
#[tokio::test]
async fn peer_request() {
    tracing_subscriber::fmt()
        .with_env_filter("tokio=trace,runtime=trace")
        .with_max_level(tracing::Level::DEBUG)
        .with_target(false)
        .compact()
        .with_file(false)
        .without_time()
        .init();

    let original_hook = std::panic::take_hook();

    let mut rng = rand::thread_rng();
    let name: String = (0..20).map(|_| rng.sample(Alphanumeric) as char).collect();
    let download_dir: String = (0..20).map(|_| rng.sample(Alphanumeric) as char).collect();
    let info_hash = [0u8; 20];
    let local_peer_id = Tracker::gen_peer_id();
    let download_dir_2 = download_dir.clone();

    std::panic::set_hook(Box::new(move |panic| {
        let _ = std::fs::remove_dir_all(&download_dir_2);
        original_hook(panic);
    }));

    create_dir_all(download_dir.clone()).unwrap();

    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(format!("{download_dir}/{name}"))
        .await
        .unwrap();

    let bytes = [3u8; 30_usize];
    file.write_all(&bytes).await.unwrap();

    let magnet = format!("magnet:?xt=urn:btih:9999999999999999999999999999999999999999&amp;dn={name}&amp;tr=udp%3A%2F%2Ftracker.coppersurfer.tk%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.openbittorrent.com%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.bittor.pw%3A1337%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.opentrackr.org%3A1337&amp;tr=udp%3A%2F%2Fbt.xxx-tracker.com%3A2710%2Fannounce&amp;tr=udp%3A%2F%2Fpublic.popcorn-tracker.org%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Feddie4.nl%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.torrent.eu.org%3A451%2Fannounce&amp;tr=udp%3A%2F%2Fp4p.arenabg.com%3A1337%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.tiny-vps.com%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Fopen.stealth.si%3A80%2Fannounce");
    let info = Info {
        file_length: Some(30),
        name,
        piece_length: 15,
        pieces: vec![0; 40],
        files: None,
    };

    let magnet = Magnet::new(&magnet).unwrap();
    let (daemon_tx, _daemon_rx) = mpsc::channel::<DaemonMsg>(1000);

    let (disk_tx, disk_rx) = mpsc::channel::<DiskMsg>(1000);
    let mut disk = Disk::new(disk_rx, download_dir.clone());

    let mut torrent = Torrent::new(disk_tx.clone(), daemon_tx.clone(), magnet.clone());
    torrent.stats.seeders = 1;
    torrent.stats.leechers = 1;
    torrent.size = info.get_size();
    torrent.have_info = true;
    torrent
        .ctx
        .has_at_least_one_piece
        .store(true, std::sync::atomic::Ordering::Relaxed);

    // pretend we already have the info,
    // that was downloaded from the magnet
    let mut torrent_info = torrent.ctx.info.write().await;
    *torrent_info = info.clone();
    drop(torrent_info);

    let mut p = torrent.ctx.bitfield.write().await;
    *p = bitvec![u8, Msb0; 0; info.pieces() as usize];
    drop(p);

    disk.new_torrent(torrent.ctx.clone()).await.unwrap();

    spawn(async move {
        disk.run().await.unwrap();
    });

    let seeder: SocketAddr = "127.0.0.1:3333".parse().unwrap();
    let torrent_ctx = torrent.ctx.clone();

    spawn(async move {
        torrent.run().await.unwrap();
    });

    let listener = TcpListener::bind(seeder).await.unwrap();

    spawn(async move {
        let torrent_ctx = torrent_ctx.clone();
        loop {
            if let Ok((socket, remote)) = listener.accept().await {
                let local = socket.local_addr().unwrap();

                let (socket, handshake) =
                    Peer::handshake(socket, Direction::Inbound, info_hash, local_peer_id)
                        .await
                        .unwrap();

                let mut peer = Peer::new(remote, torrent_ctx.clone(), handshake, local);
                peer.session.state.peer_choking = false;
                peer.session.state.am_interested = false;
                peer.have_info = true;

                let mut tick_timer = interval(Duration::from_secs(1));

                let _ = peer
                    .torrent_ctx
                    .tx
                    .send(TorrentMsg::PeerConnected(peer.ctx.id, peer.ctx.clone()))
                    .await;

                let (mut sink, mut stream) = socket.split();

                let mut n = 0;

                loop {
                    select! {
                        _ = tick_timer.tick(), if peer.have_info => {
                            peer.tick(&mut sink).await.unwrap();
                        }
                        Some(Ok(msg)) = stream.next() => {
                            match msg {
                                Message::Request(block) => {
                                    debug!("{local} received \n {block:#?} \n from {remote}");

                                    // only answer on the second request
                                    if n == 1 {
                                        let b: [u8; 15] = rand::random();
                                        sink.send(Message::Piece(Block {
                                            index: 0,
                                            begin: 0,
                                            block: b.into(),
                                        }))
                                        .await.unwrap();
                                    }
                                    if n == 2 {
                                        let b: [u8; 15] = rand::random();
                                        sink.send(Message::Piece(Block {
                                            index: 1,
                                            begin: 0,
                                            block: b.into(),
                                        }))
                                        .await.unwrap();
                                    }

                                    n += 1;
                                }
                                Message::Bitfield(field) => {
                                    debug!("{local} bitfield {field:?} {remote}");
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
        }
    });

    let mut torrent = Torrent::new(disk_tx.clone(), daemon_tx, magnet);
    torrent.size = info.get_size();

    // pretend we already have the info,
    // that was downloaded from the magnet
    let mut torrent_info = torrent.ctx.info.write().await;
    *torrent_info = info.clone();
    drop(torrent_info);

    let torrent_ctx = torrent.ctx.clone();

    spawn(async move {
        torrent.run().await.unwrap();
    });

    let socket = TcpStream::connect(seeder).await.unwrap();
    let local = socket.local_addr().unwrap();
    let local_peer_id = Tracker::gen_peer_id();

    let (socket, handshake) =
        Peer::handshake(socket, Direction::Outbound, info_hash, local_peer_id)
            .await
            .unwrap();

    // do not change the pieces here,
    // this peer does not have anything downloaded
    let mut peer = Peer::new(seeder, torrent_ctx, handshake, local);
    let tx = peer.ctx.tx.clone();
    peer.session.state.peer_choking = false;
    peer.session.state.am_interested = true;
    peer.have_info = true;

    spawn(async move {
        peer.run(Direction::Outbound, socket).await.unwrap();
    });

    tx.send(PeerMsg::RequestBlockInfos(vec![BlockInfo {
        index: 0,
        begin: 0,
        len: 15,
    }]))
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_secs(5)).await;
    std::fs::remove_dir_all(download_dir).unwrap();
}
