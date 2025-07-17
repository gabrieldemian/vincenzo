use std::fmt::Display;

use speedy::{Readable, Writable};

use crate::extensions::{core::BlockInfo, Core};

#[derive(Clone, PartialEq, Eq, Hash, Default, Readable, Writable)]
pub struct PeerId([u8; 20]);

impl Display for PeerId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", hex::encode(self.0))
    }
}

impl TryInto<PeerId> for String {
    type Error = String;
    fn try_into(self) -> Result<PeerId, Self::Error> {
        let buff = hex::decode(self).map_err(|e| e.to_string())?;
        let hash = PeerId::try_from(buff)?;
        Ok(hash)
    }
}

impl std::fmt::Debug for PeerId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = self.to_string();
        f.write_str(&s)
    }
}

impl From<PeerId> for [u8; 20] {
    fn from(value: PeerId) -> Self {
        value.0
    }
}

impl From<PeerId> for String {
    fn from(value: PeerId) -> Self {
        value.to_string()
    }
}

impl From<[u8; 20]> for PeerId {
    fn from(value: [u8; 20]) -> Self {
        Self(value)
    }
}

impl TryFrom<Vec<u8>> for PeerId {
    type Error = &'static str;

    fn try_from(value: Vec<u8>) -> Result<Self, Self::Error> {
        if value.len() != 20 {
            return Err("The PeerId must have exactly 20 bytes");
        }
        let mut buff = [0u8; 20];
        buff[..20].copy_from_slice(&value[..20]);
        Ok(PeerId(buff))
    }
}

/// Messages used to control the peer state or to make the peer forward a
/// message.
#[derive(Debug)]
pub enum PeerMsg {
    SendToSink(Core),
    /// When we download a full piece, we need to send Have's
    /// to peers that dont Have it.
    HavePiece(usize),
    /// Sometimes a peer either takes too long to answer,
    /// or simply does not answer at all. In both cases
    /// we need to request the block again.
    RequestBlockInfos(Vec<BlockInfo>),
    /// Tell this peer that we are not interested,
    /// update the local state and send a message to the peer
    NotInterested,
    /// Sends a Cancel message to cancel a block info that we
    /// expect the peer to send us, because we requested it previously.
    CancelBlock(BlockInfo),
    /// Sent when the torrent has downloaded the entire info of the torrent.
    HaveInfo,
    /// Sent when the torrent is paused, it makes the peer pause downloads and
    /// uploads
    Pause,
    /// Sent when the torrent was unpaused.
    Resume,
    /// Sent to make this peer read-only, the peer won't download
    /// anymore, but it will still seed.
    /// This usually happens when the torrent is fully downloaded.
    SeedOnly,
    /// When the program is being gracefuly shutdown, we need to kill the tokio
    /// green thread of the peer.
    Quit,
}

/// Determines who initiated the connection.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Direction {
    /// Outbound means we initiated the connection
    Outbound,
    /// Inbound means the peer initiated the connection
    Inbound,
}
