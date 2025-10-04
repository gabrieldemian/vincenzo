use tokio::sync::oneshot;
use vcz_lib::{
    disk::DiskMsg,
    error::Error,
    extensions::{BLOCK_LEN, BlockInfo},
};

#[allow(unused, dead_code)]
mod common;

#[tokio::test]
async fn request_block() -> Result<(), Error> {
    let (disk_tx, _daemon_ctx, _torrent_ctx, seeder_ctx, cleanup) =
        common::setup().await?;

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

    assert!(blocks.is_empty());

    cleanup().await;

    Ok(())
}
