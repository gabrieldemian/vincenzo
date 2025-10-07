#![feature(ip_as_octets)]

use tokio::sync::oneshot;
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
    let (seeder_ctx, cleanup) = common::setup().await?;
    let disk_tx = &seeder_ctx.torrent_ctx.disk_tx;

    let (otx, orx) = oneshot::channel();

    disk_tx
        .send(DiskMsg::RequestBlocks {
            peer_id: seeder_ctx.id.clone(),
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
    disk_tx
        .send(DiskMsg::RequestBlocks {
            peer_id: seeder_ctx.id.clone(),
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
    disk_tx
        .send(DiskMsg::RequestBlocks {
            peer_id: seeder_ctx.id.clone(),
            recipient: otx,
            qnt: 3,
        })
        .await?;

    let blocks = orx.await?;

    assert!(
        blocks.is_empty(),
        "disk must not have any more block infos to be requested"
    );

    cleanup().await;

    Ok(())
}
