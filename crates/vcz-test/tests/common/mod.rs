//! Module to share types for integration tests.
//!
//! An integration test is essentially a whole vcz client running (daemon, disk,
//! torrent, peers, tracker), sice many features are not possible to test in
//! isolation.
//!
//! Each integration test will have a "perfect" simulation of the state of a vcz
//! client: [`Daemon`], [`Disk`], etc.
//!
//! By "perfect" I mean that the setup functions will create a vcz client with
//! the exact same funtions, in the exact same way, that happens in production,
//! when the code is run "for real".

mod peer;
mod tracker;
pub(crate) use peer::*;
pub(crate) use tracker::*;

use bendy::{decoding::FromBencode, encoding::ToBencode};
use std::{
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    panic,
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::AsyncWriteExt,
    spawn,
    sync::{mpsc, oneshot},
    time::sleep,
};
use vcz_lib::{
    DISK_MSG_BOUND,
    config::{Config, ResolvedConfig},
    daemon::Daemon,
    disk::{Disk, DiskMsg, PieceStrategy, ReturnToDisk},
    error::Error,
    metainfo::MetaInfo,
    peer::{PeerCtx, PeerId},
    torrent::TorrentMsg,
};

/// Setup a torrent that is fully downloaded on disk.
async fn setup_complete_torrent() -> Result<(Disk, Daemon, MetaInfo), Error> {
    // `load_test` points to the complete location which is inside the repo.
    let mut config = Config::load_test();
    config.key = rand::random();
    setup_client(Arc::new(config)).await
}

/// Setup a torrent that doesn't have any files from the torrent.
async fn setup_incomplete_torrent() -> Result<(Disk, Daemon, MetaInfo), Error> {
    let mut config = Config::load_test();
    config.download_dir = "/tmp/fakedownload".into();
    config.metadata_dir = "/tmp/fakemetadata".into();
    config.key = rand::random();
    let r = setup_client(Arc::new(config)).await?;
    let mut p = r.0.config.metadata_dir.clone();
    // right now downloads start by adding metainfo files on the queue folder.
    p.push("queue");
    tokio::fs::create_dir_all(p.clone()).await?;
    p.push("t.torrent");
    let mut file = Disk::open_file(p).await?;
    file.write_all(&r.2.to_bencode()?).await?;
    Ok(r)
}

/// Delete download and metadata dirs of [`setup_incomplete_torrent`].
///
/// This is necessary to be more deterministic, as [`Disk`] has a side effect of
/// creating the structure of the torrent and pre-allocating files with zero
/// bytes.
fn cleanup() {
    let mut config = Config::load_test();
    config.download_dir = "/tmp/fakedownload".into();
    config.metadata_dir = "/tmp/fakemetadata".into();
    let _ = std::fs::remove_dir_all(&config.download_dir);
    let _ = std::fs::remove_dir_all(&config.metadata_dir);
}

/// Setup the boilerplate of a client.
///
/// Create a [`Disk`] and [`Daemon`] and the [`MetaInfo`] of the test torrent
/// but doesn't spawn any event loops or send any messages.
async fn setup_client(
    config: Arc<ResolvedConfig>,
) -> Result<(Disk, Daemon, MetaInfo), Error> {
    let (disk_tx, disk_rx) = mpsc::channel::<DiskMsg>(DISK_MSG_BOUND);
    let (free_tx, free_rx) = mpsc::unbounded_channel::<ReturnToDisk>();
    let daemon = Daemon::new(config.clone(), disk_tx.clone(), free_tx.clone());
    let daemon_ctx = daemon.ctx.clone();
    let disk = Disk::new(config, daemon_ctx, disk_tx, disk_rx, free_rx);

    let metainfo = MetaInfo::from_bencode(include_bytes!(
        "../../../../test-files/complete/t.torrent"
    ))?;

    Ok((disk, daemon, metainfo))
}

#[allow(dead_code, unused)]
pub struct SetupRes(
    pub (mpsc::Sender<DiskMsg>, mpsc::Sender<TorrentMsg>, Arc<PeerCtx>),
    pub (mpsc::Sender<DiskMsg>, mpsc::Sender<TorrentMsg>, Arc<PeerCtx>),
    pub (mpsc::Sender<DiskMsg>, mpsc::Sender<TorrentMsg>, Arc<PeerCtx>),
    pub (mpsc::Sender<DiskMsg>, mpsc::Sender<TorrentMsg>, Arc<PeerCtx>),
    pub (mpsc::Sender<DiskMsg>, mpsc::Sender<TorrentMsg>, Arc<PeerCtx>),
);

/// Setup boilerplate for testing.
///
/// Simulate a local leecher peer connected with 4 seeders.
#[cfg(test)]
#[allow(dead_code, unused)]
pub async fn setup_leecher_client() -> Result<(SetupRes, impl FnOnce()), Error>
{
    panic::set_hook(Box::new(|_| {
        cleanup();
    }));

    let mut tracker = MockTracker::new().await?;
    spawn(async move { tracker.run().await });

    let s1 = PeerBuilder::seeder().build().await?;
    sleep(Duration::from_millis(30)).await;
    let s2 = PeerBuilder::seeder().build().await?;
    sleep(Duration::from_millis(30)).await;
    let s3 = PeerBuilder::seeder().build().await?;
    sleep(Duration::from_millis(30)).await;
    let s4 = PeerBuilder::seeder().build().await?;
    sleep(Duration::from_millis(30)).await;
    let l1 = PeerBuilder::leecher().build().await?;

    // wait for the peers to handshake
    sleep(Duration::from_millis(50)).await;

    let s1ctx = get_peer_ctx(&l1.1, s1.0.clone()).await;
    let s2ctx = get_peer_ctx(&l1.1, s2.0.clone()).await;
    let s3ctx = get_peer_ctx(&l1.1, s3.0.clone()).await;
    let s4ctx = get_peer_ctx(&l1.1, s4.0.clone()).await;
    let l1ctx = get_peer_ctx(&s1.1, l1.0.clone()).await;

    assert_eq!(s1ctx.id, s1.0);
    assert_eq!(s2ctx.id, s2.0);
    assert_eq!(s3ctx.id, s3.0);
    assert_eq!(s4ctx.id, s4.0);
    assert_eq!(l1ctx.id, l1.0);

    let res = SetupRes(
        (l1.1, l1.2, l1ctx),
        (s1.1, s1.2, s1ctx),
        (s2.1, s2.2, s2ctx),
        (s3.1, s3.2, s3ctx),
        (s4.1, s4.2, s4ctx),
    );

    Ok((res, || cleanup()))
}

/// Setup boilerplate for testing.
///
/// Simulate a local seeder peer connected with 4 leechers.
#[cfg(test)]
#[allow(dead_code, unused)]
pub async fn setup_seeder_client() -> Result<(SetupRes, impl FnOnce()), Error> {
    panic::set_hook(Box::new(|_| {
        cleanup();
    }));

    let mut tracker = MockTracker::new().await?;

    let s1 = PeerBuilder::seeder().build().await?;
    let l1 = PeerBuilder::leecher().build().await?;
    let l2 = PeerBuilder::leecher().build().await?;
    let l3 = PeerBuilder::leecher().build().await?;
    let l4 = PeerBuilder::leecher().build().await?;

    spawn(async move { tracker.run().await });

    // wait for the peers to handshake
    sleep(Duration::from_millis(50)).await;

    let l1ctx = get_peer_ctx(&s1.1, l1.0.clone()).await;
    let l2ctx = get_peer_ctx(&s1.1, l2.0.clone()).await;
    let l3ctx = get_peer_ctx(&s1.1, l3.0.clone()).await;
    let l4ctx = get_peer_ctx(&s1.1, l4.0.clone()).await;
    let s1ctx = get_peer_ctx(&l1.1, s1.0.clone()).await;

    assert_eq!(l1ctx.id, l1.0);
    assert_eq!(l2ctx.id, l2.0);
    assert_eq!(l3ctx.id, l3.0);
    assert_eq!(l4ctx.id, l4.0);
    assert_eq!(s1ctx.id, s1.0);

    let res = SetupRes(
        (s1.1, s1.2, s1ctx),
        (l1.1, l1.2, l1ctx),
        (l2.1, l2.2, l2ctx),
        (l3.1, l3.2, l3ctx),
        (l4.1, l4.2, l4ctx),
    );

    Ok((res, || cleanup()))
}

/// From the perspective of the leecher
#[cfg(test)]
#[allow(dead_code, unused)]
pub async fn setup_pair() -> Result<
    (
        (mpsc::Sender<DiskMsg>, mpsc::Sender<TorrentMsg>, Arc<PeerCtx>),
        (mpsc::Sender<DiskMsg>, mpsc::Sender<TorrentMsg>, Arc<PeerCtx>),
        impl FnOnce(),
    ),
    Error,
> {
    panic::set_hook(Box::new(|_| {
        cleanup();
    }));

    let mut tracker = MockTracker::new().await?;
    spawn(async move { tracker.run().await });

    let s = PeerBuilder::seeder().build().await?;
    let l = PeerBuilder::leecher().build().await?;

    // wait for the peers to handshake
    sleep(Duration::from_millis(50)).await;

    let sctx = get_peer_ctx(&l.1, s.0.clone()).await;
    let lctx = get_peer_ctx(&s.1, l.0.clone()).await;

    assert_eq!(sctx.id, s.0);
    assert_eq!(lctx.id, l.0);

    Ok(((l.1, l.2, lctx), (s.1, s.2, sctx), || cleanup()))
}

async fn get_peer_ctx(
    tx: &mpsc::Sender<DiskMsg>,
    peer_id: PeerId,
) -> Arc<PeerCtx> {
    let (otx, orx) = oneshot::channel();
    let _ = tx.send(DiskMsg::GetPeerCtx { peer_id, recipient: otx }).await;
    orx.await.unwrap().unwrap()
}
