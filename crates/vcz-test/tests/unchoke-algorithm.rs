#![feature(ip_as_octets)]

use tokio::sync::oneshot;
use vcz_lib::{error::Error, torrent::TorrentMsg};

mod common;

/// Test that the unchoke algorithm works correctly.
#[tokio::test]
async fn unchoke_algorithm() -> Result<(), Error> {
    let (res, cleanup) = common::setup().await?;
    let (ldisk, ltorrent, lpeer) = res.l1;
    let (s1disk, s1torrent, s1peer) = res.s1;
    let (s2disk, s2torrent, s2peer) = res.s2;
    let (s3disk, s3torrent, s3peer) = res.s3;
    let (s4disk, s4torrent, s4peer) = res.s4;

    println!("l1 {:?}", lpeer.id);
    println!("s1 {:?}", s1peer.id);
    println!("s2 {:?}", s2peer.id);
    println!("s3 {:?}", s3peer.id);
    println!("s4 {:?}", s4peer.id);

    s1peer.counter.record_download(100_000);
    s2peer.counter.record_download(200_000);
    s3peer.counter.record_download(300_000);
    s4peer.counter.record_download(400_000);

    ltorrent.send(TorrentMsg::UnchokeAlgorithm).await?;

    let (otx, orx) = oneshot::channel();
    ltorrent.send(TorrentMsg::GetUnchokedPeers(otx)).await?;
    let unchoked = orx.await?;

    println!("len: {:?}", unchoked.len());

    for u in &unchoked {
        println!("id {:?}", u.id);
    }

    assert_eq!(unchoked[0].id, s4peer.id);
    assert_eq!(unchoked[1].id, s3peer.id);
    assert_eq!(unchoked[2].id, s2peer.id);
    panic!();

    cleanup();

    Ok(())
}
