use std::net::SocketAddr;

use bittorrent_rust::{
    bitfield::Bitfield,
    magnet_parser::{get_info_hash, get_magnet},
    peer::Peer,
    torrent::{Torrent, TorrentMsg},
};
use tokio::{net::TcpListener, spawn, sync::mpsc};

#[tokio::test]
// Test that the peer download algorithm is working
// that the peers are not taking duplicate pieces, etc
async fn piece_download_algo() {
    let (tx, rx) = mpsc::channel::<TorrentMsg>(300);
    let m = get_magnet("magnet:?xt=urn:btih:4E64AAAF48D922DBD93F8B9E4ACAA78C99BC1F40&amp;dn=MICROSOFT%20Office%20PRO%20Plus%202016%20v16.0.4266.1003%20RTM%20%2B%20Activator&amp;tr=udp%3A%2F%2Ftracker.coppersurfer.tk%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.openbittorrent.com%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.bittor.pw%3A1337%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.opentrackr.org%3A1337&amp;tr=udp%3A%2F%2Fbt.xxx-tracker.com%3A2710%2Fannounce&amp;tr=udp%3A%2F%2Fpublic.popcorn-tracker.org%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Feddie4.nl%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.torrent.eu.org%3A451%2Fannounce&amp;tr=udp%3A%2F%2Fp4p.arenabg.com%3A1337%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.tiny-vps.com%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Fopen.stealth.si%3A80%2Fannounce").unwrap();
    let _pieces = Bitfield::from(vec![0b1010_1111, 0b0011_0011]);
    let _info_hash = get_info_hash(m.xt.as_ref().unwrap());

    let mut torrent = Torrent::new(tx.clone(), rx, m).await;

    let peers: [SocketAddr; 1] = ["127.0.0.1:6881".parse().unwrap()];

    let peers: Vec<Peer> = peers
        .map(|p| Peer::from(p).torrent_ctx(torrent.ctx.clone()))
        .to_vec();

    let mut peer = peers[0].clone();

    // listen to `peers` TCP Addresses
    spawn(async move {
        let socket = TcpListener::bind(peer.addr).await.unwrap();
        let (mut socket, addr) = socket.accept().await.unwrap();

        println!("connected with {:?}", addr);
        peer.run(tx, Some(socket)).await.unwrap();
    });

    torrent.spawn_peers_tasks(peers).await.unwrap();

    spawn(async move {
        torrent.run().await.unwrap();
    })
    .await
    .unwrap();
}
