//! Helpers to build Peers.

use super::*;
use std::marker::PhantomData;
use vcz_lib::torrent::TorrentMsg;

pub(crate) trait PeerBuilderState {}

pub(crate) struct Seeder {}
impl PeerBuilderState for Seeder {}

pub(crate) struct Leecher {}
impl PeerBuilderState for Leecher {}

pub(crate) struct PeerBuilder<S: PeerBuilderState> {
    s: PhantomData<S>,
}

impl PeerBuilder<Leecher> {
    pub(crate) async fn build(
        self,
    ) -> Result<(PeerId, mpsc::Sender<DiskMsg>, mpsc::Sender<TorrentMsg>), Error>
    {
        let (mut disk, mut daemon, metainfo) =
            setup_incomplete_torrent().await?;
        let p = get_p(&daemon);
        let disk_tx = disk.tx.clone();
        spawn(async move { daemon.run().await });
        let info_hash = metainfo.info.info_hash.clone();
        disk.new_torrent_metainfo(metainfo).await?;
        let torrent_tx = disk.torrent_ctxs.get(&info_hash).unwrap().tx.clone();
        disk.set_piece_strategy(&info_hash, PieceStrategy::Sequential).await?;
        spawn(async move { disk.run().await });
        Ok((p.0, disk_tx, torrent_tx))
    }

    pub(crate) fn leecher() -> PeerBuilder<Leecher> {
        PeerBuilder { s: PhantomData }
    }
}

impl PeerBuilder<Seeder> {
    pub(crate) async fn build(
        self,
    ) -> Result<(PeerId, mpsc::Sender<DiskMsg>, mpsc::Sender<TorrentMsg>), Error>
    {
        let (mut disk, mut daemon, metainfo) = setup_complete_torrent().await?;
        let p = get_p(&daemon);
        let disk_tx = disk.tx.clone();
        spawn(async move { daemon.run().await });
        let info_hash = metainfo.info.info_hash.clone();
        disk.new_torrent_metainfo(metainfo).await?;
        let torrent_tx = disk.torrent_ctxs.get(&info_hash).unwrap().tx.clone();
        disk.set_piece_strategy(&info_hash, PieceStrategy::Sequential).await?;
        spawn(async move { disk.run().await });
        Ok((p.0, disk_tx, torrent_tx))
    }

    pub(crate) fn seeder() -> PeerBuilder<Seeder> {
        PeerBuilder { s: PhantomData }
    }
}

#[inline]
fn get_p(d: &Daemon) -> (PeerId, PeerInfo) {
    let addr = SocketAddr::V4(SocketAddrV4::new(
        Ipv4Addr::new(127, 0, 0, 1),
        d.config.local_peer_port,
    ));
    (
        d.ctx.local_peer_id.clone(),
        PeerInfo { connection_id: rand::random(), key: rand::random(), addr },
    )
}
