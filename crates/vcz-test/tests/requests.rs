#![feature(ip_as_octets)]

use std::{sync::atomic::Ordering, time::Duration};
use tokio::{sync::oneshot, time::sleep};
use vcz_lib::{
    disk::DiskMsg,
    error::Error,
    extensions::{BLOCK_LEN, BlockInfo},
    peer::PeerMsg,
    torrent::TorrentMsg,
};

mod common;

/// Simulate a local leecher requesting blocks from a seeder.
///
/// The torrent has 6 block infos, the disk must send unique block infos for
/// each request.
#[tokio::test]
async fn request_block() -> Result<(), Error> {
    let (leecher, seeder, cleanup) = common::setup_pair().await?;
    let (otx, orx) = oneshot::channel();
    let (ldisk_tx, _ltorrent, leecher) = leecher;
    let (_sdisk_tx, storrent, seeder) = seeder;

    // ! leecher and seeder are in switched perspectives.
    // but the torrent txs are in the right perspective.

    // the leecher will run the interested allgorithm against the seeder.
    seeder.tx.send(PeerMsg::InterestedAlgorithm).await?;
    sleep(Duration::from_millis(10)).await;

    // the seeder runs it's unchoke allgorithm.
    storrent.send(TorrentMsg::UnchokeAlgorithm).await?;
    sleep(Duration::from_millis(10)).await;

    // seeder is not choking the leecher
    assert!(!leecher.am_choking.load(Ordering::Acquire));
    assert!(!leecher.am_interested.load(Ordering::Acquire));
    assert!(!seeder.peer_choking.load(Ordering::Acquire));
    assert!(seeder.am_interested.load(Ordering::Acquire));

    ldisk_tx
        .send(DiskMsg::RequestBlocks {
            peer_id: seeder.id.clone(),
            recipient: otx,
            qnt: 3,
        })
        .await?;

    let blocks = orx.await?;

    assert_eq!(
        blocks,
        vec![
            BlockInfo { index: 0, begin: 0, len: BLOCK_LEN },
            BlockInfo { index: 1, begin: 0, len: BLOCK_LEN },
            BlockInfo { index: 2, begin: 0, len: BLOCK_LEN },
        ]
    );

    let (otx, orx) = oneshot::channel();
    ldisk_tx
        .send(DiskMsg::RequestBlocks {
            peer_id: seeder.id.clone(),
            recipient: otx,
            qnt: 3,
        })
        .await?;

    let blocks = orx.await?;

    assert_eq!(
        blocks,
        vec![
            BlockInfo { index: 3, begin: 0, len: BLOCK_LEN },
            BlockInfo { index: 4, begin: 0, len: BLOCK_LEN },
            BlockInfo { index: 5, begin: 0, len: BLOCK_LEN },
        ]
    );

    let (otx, orx) = oneshot::channel();
    ldisk_tx
        .send(DiskMsg::RequestBlocks {
            peer_id: seeder.id.clone(),
            recipient: otx,
            qnt: 3,
        })
        .await?;

    let blocks = orx.await?;

    assert!(
        blocks.is_empty(),
        "disk must not have any more block infos to be requested"
    );

    cleanup();

    Ok(())
}
