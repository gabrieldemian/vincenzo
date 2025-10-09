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

mod tracker;
use tracker::*;

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
    peer::PeerId,
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
/// but doesn't do anything.
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

/// Create and setup a peer belonging to the provided tracker,
/// the torrent is the one in `setup_*_torrent`.
///
/// A peer in this case is a simulation of an entire vcz client, with disk,
/// daemon, etc.
async fn setup_peer(
    tracker: &mut MockTracker,
    is_seeder: bool,
) -> Result<(PeerId, mpsc::Sender<DiskMsg>), Error> {
    // todo: craete a peer builder
    let (mut disk, mut daemon, metainfo) = if is_seeder {
        setup_complete_torrent().await?
    } else {
        setup_incomplete_torrent().await?
    };

    let peer_id = daemon.ctx.local_peer_id.clone();
    let port = daemon.config.local_peer_port;
    let addr =
        SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), port));

    let p = (
        peer_id.clone(),
        PeerInfo { connection_id: rand::random(), key: rand::random(), addr },
    );

    if is_seeder {
        tracker.insert_seeder(p);
    } else {
        tracker.insert_leecher(p);
    }

    // can only test properly with deterministic piece order.
    disk.set_piece_strategy(
        &metainfo.info.info_hash,
        PieceStrategy::Sequential,
    )?;
    let disk_tx = disk.tx.clone();
    spawn(async move { daemon.run().await });

    // simulate the leecher manually adding the torrent.
    if !is_seeder {
        disk.new_torrent_metainfo(metainfo).await?;
    }

    spawn(async move { disk.run().await });

    Ok((peer_id, disk_tx))
}

/// Setup boilerplate for testing.
///
/// Simulate a local leecher peer connected with a seeder peer.
#[cfg(test)]
pub async fn setup()
-> Result<(mpsc::Sender<DiskMsg>, PeerId, impl FnOnce()), Error> {
    use std::{panic, time::Duration};
    use tokio::time::sleep;

    panic::set_hook(Box::new(|_| {
        cleanup();
    }));

    let mut tracker = MockTracker::new().await?;

    // this is the local peer, which is a leecher
    let (_leecher_id, disk_tx) = setup_peer(&mut tracker, false).await?;
    let (seeder_id, _) = setup_peer(&mut tracker, true).await?;

    spawn(async move { tracker.run().await });

    // wait for the peers to handshake
    sleep(Duration::from_millis(200)).await;

    Ok((disk_tx, seeder_id, || cleanup()))
}
