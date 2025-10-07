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
//!
//! With that being said, a tracker mock is still missing.

mod tracker;
use tracker::*;

use bendy::decoding::FromBencode;
use futures::StreamExt;
use std::{
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    sync::Arc,
};
use tokio::{
    net::{TcpListener, TcpStream},
    spawn,
    sync::{Mutex, broadcast, mpsc},
    time::Instant,
};
use tokio_util::codec::Framed;
use vcz_lib::{
    bitfield::{Bitfield, Reserved, VczBitfield},
    config::{Config, ResolvedConfig},
    counter::Counter,
    daemon::{Daemon, DaemonMsg},
    disk::{Disk, DiskMsg, PieceStrategy, ReturnToDisk},
    error::Error,
    extensions::CoreCodec,
    metainfo::MetaInfo,
    peer::{
        self, DEFAULT_REQUEST_QUEUE_LEN, Peer, PeerCtx, PeerId, PeerMsg,
        RequestManager,
    },
    torrent::{self, FromMetaInfo, PeerBrMsg, Torrent, TorrentCtx, TorrentMsg},
};

/// Setup a torrent that is fully downloaded on disk.
async fn setup_complete_torrent()
-> Result<(Disk, Daemon, Arc<TorrentCtx>, usize, impl AsyncFnOnce()), Error> {
    // `load_test` points to the complete location
    let mut config = Config::load_test();
    config.key = 0;
    setup_torrent(Arc::new(config), true).await
}

/// Setup a torrent that doesn't have any files from the torrent.
async fn setup_incomplete_torrent()
-> Result<(Disk, Daemon, Arc<TorrentCtx>, usize, impl AsyncFnOnce()), Error> {
    let mut config = Config::load_test();
    // points to a place that doesn't exist
    config.download_dir = "/tmp/fakedownload".into();
    config.metadata_dir = "/tmp/fakemetadata".into();
    config.key = 1;
    setup_torrent(Arc::new(config), false).await
}

/// Delete download and metadata dirs.
///
/// This is necessary to be more deterministic, as [`Disk`] has a side effect of
/// creating the structure of the torrent and pre-allocating files with zero
/// bytes.
async fn cleanup(config: Arc<ResolvedConfig>) {
    let _ = tokio::fs::remove_dir_all(&config.download_dir).await;
    let _ = tokio::fs::remove_dir_all(&config.metadata_dir).await;
}

/// Setup the boilerplate of a torrent.
///
/// The event loops for [`Disk`], [`Daemon`] and [`Torrent`] (through Daemon)
/// are run. But there are no peers and no trackers.
async fn setup_torrent(
    config: Arc<ResolvedConfig>,
    is_complete: bool,
) -> Result<(Disk, Daemon, Arc<TorrentCtx>, usize, impl AsyncFnOnce()), Error> {
    let (disk_tx, disk_rx) = mpsc::channel::<DiskMsg>(10);
    let (free_tx, free_rx) = mpsc::unbounded_channel::<ReturnToDisk>();
    let daemon = Daemon::new(config.clone(), disk_tx.clone(), free_tx.clone());
    let daemon_ctx = daemon.ctx.clone();

    let mut disk = Disk::new(
        config.clone(),
        daemon_ctx.clone(),
        disk_tx.clone(),
        disk_rx,
        free_rx,
    );

    let metainfo = MetaInfo::from_bencode(include_bytes!(
        "../../../../test-files/complete/t.torrent"
    ))?;

    let info_hash = metainfo.info.info_hash.clone();
    let (btx, _brx) = broadcast::channel::<PeerBrMsg>(10);
    let (tx, rx) = mpsc::channel::<TorrentMsg>(10);

    let torrent_ctx = Arc::new(TorrentCtx {
        btx,
        free_tx,
        disk_tx: disk.tx.clone(),
        tx,
        info_hash,
    });

    let cc = config.clone();
    let pieces_count = metainfo.info.pieces();

    let torrent = Torrent {
        config,
        source: FromMetaInfo { meta_info: metainfo.clone() },
        state: torrent::Idle {
            metadata_size: Some(metainfo.info.metadata_size),
        },
        bitfield: if is_complete {
            !Bitfield::from_piece(pieces_count)
        } else {
            Bitfield::from_piece(pieces_count)
        },
        ctx: torrent_ctx.clone(),
        daemon_ctx,
        name: "t".into(),
        rx,
        status: torrent::TorrentStatus::Downloading,
    };

    let torrent_ctx = torrent.ctx.clone();
    disk.new_torrent_metainfo(metainfo).await?;
    let _ =
        disk.daemon_ctx.tx.send(DaemonMsg::AddTorrentMetaInfo(torrent)).await;

    Ok((disk, daemon, torrent_ctx, pieces_count, move || cleanup(cc)))
}

/// Create a [`Peer`] belonging to the provided torrent.
async fn setup_peer(
    torrent_ctx: Arc<TorrentCtx>,
) -> Result<Peer<peer::Connected>, Error> {
    let (tx, rx) = mpsc::channel::<PeerMsg>(10);
    let free_tx = torrent_ctx.free_tx.clone();

    let peer_ctx = Arc::new(PeerCtx {
        torrent_ctx,
        remote_addr: SocketAddr::V4(SocketAddrV4::new(
            Ipv4Addr::new(127, 0, 0, 1),
            0000,
        )),
        local_addr: SocketAddr::V4(SocketAddrV4::new(
            Ipv4Addr::new(127, 0, 0, 1),
            0000,
        )),
        counter: Counter::default(),
        last_download_rate_update: Mutex::new(Instant::now()),
        tx,
        peer_interested: true.into(),
        id: PeerId::generate(),
        am_interested: true.into(),
        am_choking: false.into(),
        peer_choking: false.into(),
        direction: peer::Direction::Outbound,
    });

    let listener = TcpListener::bind("0.0.0.0:0").await?;
    let local_addr = listener.local_addr()?;
    let stream = TcpStream::connect(local_addr).await?;

    // let (socket, _) = listener.accept().await?;
    let socket = Framed::new(stream, CoreCodec);
    let (sink, stream) = socket.split();

    let peer = Peer::<peer::Connected> {
        state_log: Default::default(),
        state: peer::Connected {
            free_tx,
            is_paused: false,
            ctx: peer_ctx.clone(),
            ext_states: peer::ExtStates::default(),
            have_info: true,
            in_endgame: false,
            incoming_requests: Vec::with_capacity(10),
            req_man_meta: RequestManager::new(),
            req_man_block: RequestManager::new(),
            reserved: Reserved::default(),
            rx,
            seed_only: false,
            sink,
            stream,
            target_request_queue_len: DEFAULT_REQUEST_QUEUE_LEN,
        },
    };

    Ok(peer)
}

/// Setup boilerplate for testing.
///
/// Simulate a local leecher peer connected with a seeder peer.
#[cfg(test)]
pub async fn setup() -> Result<(Arc<PeerCtx>, impl AsyncFnOnce()), Error> {
    // remote seeder
    let (
        mut seeder_disk,
        mut daemon,
        seeder_torrent_ctx,
        pieces_count,
        cleanup,
    ) = setup_incomplete_torrent().await?;

    let info_hash = seeder_torrent_ctx.info_hash.clone();
    let seeder = setup_peer(seeder_torrent_ctx.clone()).await?;
    let seeder_ctx = seeder.state.ctx.clone();
    let seeder_pieces = !Bitfield::from_piece(pieces_count);

    seeder_torrent_ctx
        .tx
        .send(TorrentMsg::AddConnectedPeers(vec![(
            seeder.state.ctx.clone(),
            seeder_pieces,
        )]))
        .await?;
    seeder_disk.new_peer(seeder.state.ctx.clone());
    seeder_disk.set_piece_strategy(&info_hash, PieceStrategy::Sequential)?;

    spawn(async move { daemon.run().await });
    spawn(async move { seeder_disk.run().await });
    // spawn(async move {
    //     let _ = seeder.run().await;
    // });

    Ok((seeder_ctx, cleanup))
}
