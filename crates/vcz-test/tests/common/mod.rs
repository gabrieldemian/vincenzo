use futures::StreamExt;
use hashbrown::HashSet;
use std::{
    net::{Ipv4Addr, SocketAddrV4},
    time::Duration,
};

use tokio::{
    sync::{Mutex, broadcast},
    time::{Instant, interval, interval_at},
};
use tokio_util::codec::Framed;
#[cfg(test)]
use vcz_lib::daemon::DaemonCtx;

use vcz_lib::{
    bitfield::{Bitfield, Reserved, VczBitfield},
    config::{Config, ResolvedConfig},
    counter::Counter,
    daemon::{Daemon, DaemonMsg},
    disk::{Disk, DiskMsg, ReturnToDisk},
    extensions::{CoreCodec, core::BLOCK_LEN},
    peer::{self, DEFAULT_REQUEST_QUEUE_LEN, Peer, PeerMsg, RequestManager},
    torrent::{
        self, Connected, FromMetaInfo, PeerBrMsg, Stats, Torrent, TorrentMsg,
    },
    tracker::TrackerMsg,
};

use bendy::decoding::FromBencode;
use hashbrown::HashMap;
use std::{collections::BTreeMap, net::SocketAddr, sync::Arc};
use tokio::{
    net::{TcpListener, TcpStream},
    spawn,
    sync::mpsc,
};
use vcz_lib::{
    disk::PieceStrategy,
    error::Error,
    metainfo::MetaInfo,
    peer::{PeerCtx, PeerId},
    torrent::TorrentCtx,
};

async fn setup_complete_torrent()
-> (Disk, Daemon, Arc<TorrentCtx>, usize, impl AsyncFnOnce()) {
    let mut config = Config::load_test();
    config.key = 0;
    setup_torrent(Arc::new(config), true).await
}

async fn setup_incomplete_torrent()
-> (Disk, Daemon, Arc<TorrentCtx>, usize, impl AsyncFnOnce()) {
    let mut config = Config::load_test();
    config.download_dir = "/tmp/fakedownload".into();
    config.metadata_dir = "/tmp/fakemetadata".into();
    config.key = 1;
    setup_torrent(Arc::new(config), true).await
}

pub async fn cleanup(config: Arc<ResolvedConfig>) {
    let _ = tokio::fs::remove_dir_all(&config.download_dir).await;
    let _ = tokio::fs::remove_dir_all(&config.metadata_dir).await;
}

/// Setup the boilerplate of a complete torrent, disk, daemon, and torrent
/// structs. Peers are not created and event loops not run.
async fn setup_torrent(
    config: Arc<ResolvedConfig>,
    is_complete: bool,
) -> (Disk, Daemon, Arc<TorrentCtx>, usize, impl AsyncFnOnce()) {
    let name = "t".to_string();

    let (disk_tx, disk_rx) = mpsc::channel::<DiskMsg>(10);
    let (free_tx, free_rx) = mpsc::unbounded_channel::<ReturnToDisk>();

    let mut daemon =
        Daemon::new(config.clone(), disk_tx.clone(), free_tx.clone());
    let daemon_ctx = daemon.ctx.clone();

    let mut disk = Disk::new(
        config.clone(),
        daemon_ctx.clone(),
        disk_tx.clone(),
        disk_rx,
        free_rx,
    );

    let (tracker_tx, _tracker_rx) = broadcast::channel::<TrackerMsg>(10);
    let (tx, rx) = mpsc::channel::<TorrentMsg>(10);

    let metainfo = MetaInfo::from_bencode(include_bytes!(
        "../../../../test-files/complete/t.torrent"
    ))
    .unwrap();

    let pieces_count = metainfo.info.pieces();
    let info_hash = metainfo.info.info_hash.clone();
    let metadata_size = metainfo.info.metadata_size;
    let (btx, _brx) = broadcast::channel::<PeerBrMsg>(10);

    let torrent_ctx = Arc::new(TorrentCtx {
        btx,
        free_tx,
        disk_tx: disk.tx.clone(),
        tx,
        info_hash,
    });

    // try to reconnect with errored peers
    let reconnect_interval = interval(Duration::from_secs(2));

    // send state to the frontend, if connected.
    let heartbeat_interval = interval(Duration::from_secs(1));

    let log_rates_interval = interval(Duration::from_secs(5));

    // unchoke the slowest interested peer.
    let optimistic_unchoke_interval = interval(Duration::from_secs(30));

    // unchoke algorithm:
    // - choose the best 3 interested uploaders and unchoke them.
    let unchoke_interval = interval_at(
        Instant::now() + Duration::from_secs(10),
        Duration::from_secs(10),
    );

    let cc = config.clone();
    let torrent = Torrent {
        config,
        source: FromMetaInfo { meta_info: metainfo.clone() },
        state: torrent::Idle {
            metadata_size: Some(metainfo.info.metadata_size),
        },
        bitfield: if is_complete {
            Bitfield::from_piece(pieces_count)
        } else {
            !Bitfield::from_piece(pieces_count)
        },
        ctx: torrent_ctx.clone(),
        daemon_ctx,
        name,
        rx,
        status: torrent::TorrentStatus::Downloading,
    };

    let torrent_ctx = torrent.ctx.clone();
    disk.new_torrent_metainfo(metainfo).await.unwrap();
    disk.daemon_ctx.tx.send(DaemonMsg::AddTorrentMetaInfo(torrent)).await;

    (disk, daemon, torrent_ctx, pieces_count, move || cleanup(cc))
}

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

    let (_socket, _) = listener.accept().await.unwrap();
    let socket = Framed::new(stream, CoreCodec);
    let (sink, stream) = socket.split();

    let mut peer = Peer::<peer::Connected> {
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

/// Setup boilerplate for testing,
/// this will load the torrent of `t.torrent`. Disk, Daemon, and Torrent structs
/// and will also create 5 peers.
#[cfg(test)]
pub async fn setup() -> Result<
    (
        mpsc::Sender<DiskMsg>,
        Arc<DaemonCtx>,
        Arc<TorrentCtx>,
        Arc<PeerCtx>,
        impl AsyncFnOnce(),
    ),
    Error,
> {
    // leecher
    let (
        mut leecher_disk,
        mut daemon,
        leecher_torrent_ctx,
        pieces_count,
        cleanup,
    ) = setup_incomplete_torrent().await;

    let info_hash = leecher_torrent_ctx.info_hash.clone();
    let mut leecher = setup_peer(leecher_torrent_ctx.clone()).await?;
    let leecher_ctx = leecher.state.ctx.clone();
    let leecher_pieces = !Bitfield::from_piece(pieces_count);

    leecher_torrent_ctx
        .tx
        .send(TorrentMsg::AddConnectedPeers(vec![(
            leecher.state.ctx.clone(),
            leecher_pieces,
        )]))
        .await?;
    leecher_disk.new_peer(leecher.state.ctx.clone());
    leecher_disk.set_piece_strategy(&info_hash, PieceStrategy::Sequential);

    let disk_tx = leecher_disk.tx.clone();
    let daemon_ctx = daemon.ctx.clone();

    spawn(async move { daemon.run().await });
    spawn(async move { leecher_disk.run().await });
    // spawn(async move {
    //     let _ = leecher.run().await;
    // });

    // seeder

    Ok((disk_tx, daemon_ctx, leecher_torrent_ctx, leecher_ctx, cleanup))
}
