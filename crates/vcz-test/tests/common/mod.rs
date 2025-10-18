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

use bendy::decoding::FromBencode;
use std::{
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    sync::Arc,
};
use tokio::{spawn, sync::mpsc};
use vcz_lib::{
    config::{Config, ResolvedConfig},
    daemon::Daemon,
    disk::{Disk, DiskMsg, PieceStrategy, ReturnToDisk},
    error::Error,
    metainfo::MetaInfo,
    peer::{PeerCtx, PeerId, PeerMsg},
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
    // points to a place that doesn't exist
    config.download_dir = "/tmp/fakedownload".into();
    config.metadata_dir = "/tmp/fakemetadata".into();
    config.key = rand::random();
    setup_client(Arc::new(config)).await
}

/// Delete download and metadata dirs of [`setup_incomplete_torrent`].
///
/// This is necessary to be more deterministic, as [`Disk`] has a side effect of
/// creating the structure of the torrent and pre-allocating files with zero
/// bytes.
fn cleanup() {
    let mut cc = Config::load_test();
    cc.download_dir = "/tmp/fakedownload".into();
    cc.metadata_dir = "/tmp/fakemetadata".into();
    let _ = std::fs::remove_dir_all(&cc.download_dir);
    let _ = std::fs::remove_dir_all(&cc.metadata_dir);
}

/// Setup the boilerplate of a client.
///
/// Create a [`Disk`] and [`Daemon`] and the [`MetaInfo`] of the test torrent
/// but doesn't spawn any event loops or send any messages.
async fn setup_client(
    config: Arc<ResolvedConfig>,
) -> Result<(Disk, Daemon, MetaInfo), Error> {
    let (disk_tx, disk_rx) = mpsc::channel::<DiskMsg>(10);
    let (free_tx, free_rx) = mpsc::unbounded_channel::<ReturnToDisk>();

    let daemon = Daemon::new(config.clone(), disk_tx.clone(), free_tx.clone());
    let daemon_ctx = daemon.ctx.clone();
    let disk = Disk::new(config, daemon_ctx, disk_tx, disk_rx, free_rx);

    let metainfo = MetaInfo::from_bencode(include_bytes!(
        "../../../../test-files/complete/t.torrent"
    ))?;

    Ok((disk, daemon, metainfo))
}

pub struct SetupRes {
    pub l1:
        (PeerId, mpsc::Sender<DiskMsg>, mpsc::Sender<TorrentMsg>, Arc<PeerCtx>),
    pub s1:
        (PeerId, mpsc::Sender<DiskMsg>, mpsc::Sender<TorrentMsg>, Arc<PeerCtx>),
}

/// Setup boilerplate for testing.
///
/// Simulate a local leecher peer connected with a seeder peer.
#[cfg(test)]
pub async fn setup() -> Result<(SetupRes, impl FnOnce()), Error> {
    use std::{panic, time::Duration};
    use tokio::{sync::oneshot, time::sleep};

    panic::set_hook(Box::new(|_| {
        cleanup();
    }));

    let mut tracker = MockTracker::new().await?;

    let l1 = PeerBuilder::new().build(&mut tracker).await?;
    let s1 = PeerBuilder::new_seeder().build(&mut tracker).await?;

    spawn(async move { tracker.run().await });

    // wait for the peers to handshake
    sleep(Duration::from_millis(100)).await;

    let (otx, orx) = oneshot::channel();
    let _ = l1
        .1
        .send(DiskMsg::GetPeerCtx { peer_id: s1.0.clone(), recipient: otx })
        .await;
    let l1tx = orx.await?.unwrap();

    let (otx, orx) = oneshot::channel();
    let _ = s1
        .1
        .send(DiskMsg::GetPeerCtx { peer_id: l1.0.clone(), recipient: otx })
        .await;
    let s1tx = orx.await?.unwrap();

    let res =
        SetupRes { l1: (l1.0, l1.1, l1.2, l1tx), s1: (s1.0, s1.1, s1.2, s1tx) };

    Ok((res, || cleanup()))
}
