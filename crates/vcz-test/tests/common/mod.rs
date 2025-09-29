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
    bitfield::Reserved,
    counter::Counter,
    daemon::Daemon,
    disk::{Disk, DiskMsg, ReturnToDisk},
    extensions::{CoreCodec, core::BLOCK_LEN},
    peer::{self, DEFAULT_REQUEST_QUEUE_LEN, Peer, PeerMsg, RequestManager},
    torrent::{Connected, FromMetaInfo, PeerBrMsg, Stats, Torrent, TorrentMsg},
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

/// Setup the boilerplate of a complete torrent, disk, daemon, and torrent
/// structs. Peers are not created and event loops not run.
async fn setup_complete_torrent()
-> (Disk, Daemon, Torrent<Connected, FromMetaInfo>, usize) {
    let name = "t".to_string();
    let (disk_tx, disk_rx) = mpsc::channel::<DiskMsg>(10);
    let (free_tx, free_rx) = mpsc::unbounded_channel::<ReturnToDisk>();

    let mut daemon = Daemon::new(disk_tx.clone(), free_tx.clone());
    let daemon_ctx = daemon.ctx.clone();

    let mut disk =
        Disk::new(daemon_ctx.clone(), disk_tx.clone(), disk_rx, free_rx);

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

    let mut torrent = Torrent {
        source: FromMetaInfo { meta_info: metainfo.clone() },
        state: Connected {
            reconnect_interval,
            heartbeat_interval,
            log_rates_interval,
            optimistic_unchoke_interval,
            unchoke_interval,
            size: 98304,
            counter: Counter::new(),
            unchoked_peers: Vec::new(),
            opt_unchoked_peer: None,
            peer_pieces: Default::default(),
            connecting_peers: Vec::new(),
            error_peers: Vec::new(),
            stats: Stats::default(),
            idle_peers: HashSet::new(),
            tracker_tx: tracker_tx.clone(),
            metadata_size: Some(metadata_size),
            connected_peers: Default::default(),
            info_pieces: BTreeMap::new(),
        },
        bitfield: bitvec::bitvec![u8, bitvec::prelude::Msb0; 1; pieces_count],
        ctx: torrent_ctx.clone(),
        daemon_ctx,
        name,
        rx,
        status: vcz_lib::torrent::TorrentStatus::Downloading,
    };

    disk.new_torrent_metainfo(metainfo).await.unwrap();

    (disk, daemon, torrent, pieces_count)
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
    (mpsc::Sender<DiskMsg>, Arc<DaemonCtx>, Arc<TorrentCtx>, Arc<PeerCtx>),
    Error,
> {
    use vcz_lib::{
        bitfield::{Bitfield, VczBitfield},
        config::CONFIG,
    };

    let (mut disk, mut daemon, mut torrent, pieces_count) =
        setup_complete_torrent().await;

    let torrent_ctx = torrent.ctx.clone();
    let info_hash = torrent_ctx.info_hash.clone();

    let mut leecher = setup_peer(torrent_ctx.clone()).await?;
    let leecher_ctx = leecher.state.ctx.clone();
    let leecher_pieces = Bitfield::from_piece(pieces_count);

    torrent
        .state
        .peer_pieces
        .insert(leecher.state.ctx.id.clone(), leecher_pieces);
    torrent.state.connected_peers.push(leecher.state.ctx.clone());

    disk.piece_strategy.insert(info_hash.clone(), PieceStrategy::Sequential);
    disk.new_peer(leecher.state.ctx.clone());

    let disk_tx = disk.tx.clone();
    let daemon_ctx = daemon.ctx.clone();

    spawn(async move { daemon.run().await });
    spawn(async move { disk.run().await });
    spawn(async move { torrent.run().await });
    spawn(async move {
        let _ = leecher.run().await;
    });

    Ok((disk_tx, daemon_ctx, torrent_ctx, leecher_ctx))
}
