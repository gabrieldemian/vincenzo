#![feature(ip_as_octets)]

use std::time::Duration;

use tokio::{sync::oneshot, time::sleep};
use vcz_lib::{
    disk::DiskMsg,
    error::Error,
    extensions::{BLOCK_LEN, BlockInfo},
};

mod common;

/// Simulate a local leecher requesting blocks from a seeder.
///
/// The torrent has 6 block infos, the disk must send unique block infos for
/// each request.
#[tokio::test]
async fn request_block() -> Result<(), Error> {
    let (res, cleanup) = common::setup_leecher_client().await?;
    let (otx, orx) = oneshot::channel();
    let (ldisk_tx, ..) = res.l1;
    let (.., sctx) = res.s1;

    sleep(Duration::from_millis(100)).await;

    ldisk_tx
        .send(DiskMsg::RequestBlocks {
            peer_id: sctx.id.clone(),
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
            peer_id: sctx.id.clone(),
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
            peer_id: sctx.id.clone(),
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
