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
async fn download() -> Result<(), Error> {
    let (leecher, seeder, cleanup) = common::setup_pair().await?;
    let (ldisk_tx, _ltorrent, leecher) = leecher;
    let (_sdisk_tx, storrent, seeder) = seeder;

    // ! leecher and seeder are in switched perspectives.
    // but the torrent txs are in the right perspective.

    // the leecher runs the interested allgorithm against the seeder.
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

    // leecher requests blocks from the seeder
    seeder.tx.send(PeerMsg::RequestAlgorithm).await?;
    sleep(Duration::from_millis(10)).await;

    // leecher should have received the blocks and written to disk.
    // `ReadBlock` reads it from disk, so we know the download worked.
    let (otx, orx) = oneshot::channel();
    ldisk_tx
        .send(DiskMsg::ReadBlock {
            block_info: BlockInfo::new(0, 0, BLOCK_LEN),
            info_hash: leecher.torrent_ctx.info_hash.clone(),
            recipient: otx,
        })
        .await?;

    let block = orx.await?;
    assert_eq!(*block.first().unwrap(), 1);
    assert_eq!(block.len(), BLOCK_LEN);

    // these files are from `test-files/t/*`.
    // each file has 2 blocks.
    // foo.txt and baz.txt = 0x01
    // bee.txt = 0x02

    let (otx, orx) = oneshot::channel();
    ldisk_tx
        .send(DiskMsg::ReadBlock {
            block_info: BlockInfo::new(1, 0, BLOCK_LEN),
            info_hash: leecher.torrent_ctx.info_hash.clone(),
            recipient: otx,
        })
        .await?;

    let block = orx.await?;
    assert_eq!(*block.first().unwrap(), 1);
    assert_eq!(block.len(), BLOCK_LEN);

    let (otx, orx) = oneshot::channel();
    ldisk_tx
        .send(DiskMsg::ReadBlock {
            block_info: BlockInfo::new(2, 0, BLOCK_LEN),
            info_hash: leecher.torrent_ctx.info_hash.clone(),
            recipient: otx,
        })
        .await?;

    let block = orx.await?;
    assert_eq!(*block.first().unwrap(), 1);
    assert_eq!(block.len(), BLOCK_LEN);

    let (otx, orx) = oneshot::channel();
    ldisk_tx
        .send(DiskMsg::ReadBlock {
            block_info: BlockInfo::new(3, 0, BLOCK_LEN),
            info_hash: leecher.torrent_ctx.info_hash.clone(),
            recipient: otx,
        })
        .await?;

    let block = orx.await?;
    assert_eq!(*block.first().unwrap(), 1);
    assert_eq!(block.len(), BLOCK_LEN);

    let (otx, orx) = oneshot::channel();
    ldisk_tx
        .send(DiskMsg::ReadBlock {
            block_info: BlockInfo::new(4, 0, BLOCK_LEN),
            info_hash: leecher.torrent_ctx.info_hash.clone(),
            recipient: otx,
        })
        .await?;

    let block = orx.await?;
    assert_eq!(*block.first().unwrap(), 2);
    assert_eq!(block.len(), BLOCK_LEN);

    let (otx, orx) = oneshot::channel();
    ldisk_tx
        .send(DiskMsg::ReadBlock {
            block_info: BlockInfo::new(5, 0, BLOCK_LEN),
            info_hash: leecher.torrent_ctx.info_hash.clone(),
            recipient: otx,
        })
        .await?;

    let block = orx.await?;
    assert_eq!(*block.first().unwrap(), 2);
    assert_eq!(block.len(), BLOCK_LEN);

    sleep(Duration::from_millis(10)).await;

    cleanup();

    Ok(())
}
