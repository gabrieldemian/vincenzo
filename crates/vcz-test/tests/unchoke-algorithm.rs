#![feature(ip_as_octets)]

use std::{sync::atomic::Ordering, time::Duration};
use tokio::{sync::oneshot, time::sleep};
use vcz_lib::{error::Error, torrent::TorrentMsg};

mod common;

/// Test that the unchoke algorithm works correctly.
#[tokio::test]
async fn unchoke_algorithm() -> Result<(), Error> {
    let (res, cleanup) = common::setup_seeder_client().await?;
    let (.., storrent, _speer) = res.0;
    let (.., l1peer) = res.1;
    let (.., l2peer) = res.2;
    let (.., l3peer) = res.3;
    let (.., l4peer) = res.4;

    l1peer.peer_interested.store(true, Ordering::Relaxed);
    l2peer.peer_interested.store(true, Ordering::Relaxed);
    l3peer.peer_interested.store(true, Ordering::Relaxed);
    l4peer.peer_interested.store(true, Ordering::Relaxed);

    l1peer.counter.record_download(100_000);
    l2peer.counter.record_download(200_000);
    l3peer.counter.record_download(300_000);
    l4peer.counter.record_download(400_000);
    l1peer.counter.update_rates();
    l2peer.counter.update_rates();
    l3peer.counter.update_rates();
    l4peer.counter.update_rates();

    storrent.send(TorrentMsg::UnchokeAlgorithm).await?;
    sleep(Duration::from_millis(30)).await;

    let (otx, orx) = oneshot::channel();
    storrent.send(TorrentMsg::GetUnchokedPeers(otx)).await?;
    let unchoked = orx.await?;

    assert_eq!(unchoked[0].id, l4peer.id);
    assert_eq!(unchoked[1].id, l3peer.id);
    assert_eq!(unchoked[2].id, l2peer.id);

    // l4 is no longer in the top3, it should be replaced.

    l1peer.counter.record_download(900_000_000);
    l2peer.counter.record_download(800_000_000);
    l3peer.counter.record_download(700_000_000);
    l4peer.counter.record_download(100_000);
    l1peer.counter.update_rates();
    l2peer.counter.update_rates();
    l3peer.counter.update_rates();
    l4peer.counter.update_rates();

    storrent.send(TorrentMsg::UnchokeAlgorithm).await?;
    sleep(Duration::from_millis(30)).await;

    let (otx, orx) = oneshot::channel();
    storrent.send(TorrentMsg::GetUnchokedPeers(otx)).await?;
    let unchoked = orx.await?;

    assert_eq!(unchoked[0].id, l1peer.id);
    assert_eq!(unchoked[1].id, l2peer.id);
    assert_eq!(unchoked[2].id, l3peer.id);

    cleanup();

    Ok(())
}
