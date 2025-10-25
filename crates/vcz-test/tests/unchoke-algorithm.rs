#![feature(ip_as_octets)]

use std::time::Duration;

use tokio::{sync::oneshot, time::sleep};
use vcz_lib::{error::Error, torrent::TorrentMsg};

mod common;

/// Test that the unchoke algorithm works correctly.
#[tokio::test]
async fn unchoke_algorithm() -> Result<(), Error> {
    let (res, cleanup) = common::setup_leecher_client().await?;
    let (ldisk, ltorrent, lpeer) = res.l1;
    let (s1disk, s1torrent, s1peer) = res.s1;
    let (s2disk, s2torrent, s2peer) = res.s2;
    let (s3disk, s3torrent, s3peer) = res.s3;
    let (s4disk, s4torrent, s4peer) = res.s4;

    s1peer.counter.record_download(100_000);
    s2peer.counter.record_download(200_000);
    s3peer.counter.record_download(300_000);
    s4peer.counter.record_download(400_000);
    s1peer.counter.update_rates();
    s2peer.counter.update_rates();
    s3peer.counter.update_rates();
    s4peer.counter.update_rates();

    ltorrent.send(TorrentMsg::UnchokeAlgorithm).await?;
    sleep(Duration::from_millis(100)).await;
    let (otx, orx) = oneshot::channel();
    ltorrent.send(TorrentMsg::GetUnchokedPeers(otx)).await?;
    let unchoked = orx.await?;

    assert_eq!(unchoked[0].id, s4peer.id);
    assert_eq!(unchoked[1].id, s3peer.id);
    assert_eq!(unchoked[2].id, s2peer.id);

    // s4 is no longer in the top3, it should be replaced.

    s1peer.counter.record_download(900_000_000);
    s2peer.counter.record_download(800_000_000);
    s3peer.counter.record_download(700_000_000);
    s4peer.counter.record_download(100_000);
    s1peer.counter.update_rates();
    s2peer.counter.update_rates();
    s3peer.counter.update_rates();
    s4peer.counter.update_rates();

    ltorrent.send(TorrentMsg::UnchokeAlgorithm).await?;
    sleep(Duration::from_millis(100)).await;
    let (otx, orx) = oneshot::channel();
    ltorrent.send(TorrentMsg::GetUnchokedPeers(otx)).await?;
    let unchoked = orx.await?;

    assert_eq!(unchoked[0].id, s1peer.id);
    assert_eq!(unchoked[1].id, s2peer.id);
    assert_eq!(unchoked[2].id, s3peer.id);

    cleanup();

    Ok(())
}
