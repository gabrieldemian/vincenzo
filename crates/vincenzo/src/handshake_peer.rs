use std::sync::Arc;

use bendy::encoding::ToBencode;
use bitvec::{array::BitArray, order::Msb0};
use futures::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_util::codec::{Framed, FramedParts};
use tracing::{debug, warn};

use crate::{
    bitfield::Reserved,
    error::Error,
    extensions::{
        extended::{codec::Extended, Extension},
        Core, ExtendedMessage, Handshake, HandshakeCodec, IntoExtendedMessage,
        Message, MessageCodec,
    },
    peer::{Direction, PeerId},
    torrent::TorrentCtx,
};

/// The requirements to begin a handshake with a peer
#[derive(Default)]
pub struct PeerBuilder {
    direction: Option<Direction>,
    torrent_ctx: Option<Arc<TorrentCtx>>,
    socket: Option<TcpStream>,
    local_peer_id: Option<PeerId>,
}

impl PeerBuilder {
    pub fn direction(mut self, v: Direction) -> Self {
        self.direction = Some(v);
        self
    }
    pub fn torrent_ctx(mut self, v: Arc<TorrentCtx>) -> Self {
        self.torrent_ctx = Some(v);
        self
    }
    pub fn socket(mut self, v: TcpStream) -> Self {
        self.socket = Some(v);
        self
    }
    pub fn local_peer_id(mut self, v: PeerId) -> Self {
        self.local_peer_id = Some(v);
        self
    }
    pub async fn handshake(self) -> Result<HandshakedPeer, Error> {
        if self.direction.is_none()
            || self.torrent_ctx.is_none()
            || self.socket.is_none()
            || self.local_peer_id.is_none()
        {
            return Err(Error::Timeout);
        }

        let socket = self.socket.unwrap();
        let local_peer_id = self.local_peer_id.unwrap();
        let torrent_ctx = self.torrent_ctx.unwrap();
        let local = socket.local_addr()?;
        let remote = socket.peer_addr()?;

        let info_hash = torrent_ctx.info_hash.clone();

        let mut socket = Framed::new(socket, HandshakeCodec);

        let our_handshake = Handshake::new(info_hash, local_peer_id.clone());
        let peer_handshake: Handshake;

        // if we are connecting, send the first handshake
        if self.direction.unwrap() == Direction::Outbound {
            debug!("{local} sending the first handshake to {remote}");
            socket.send(our_handshake.clone()).await?;
        }

        // wait for, and validate, their handshake
        if let Some(Ok(their_handshake)) = socket.next().await {
            debug!("{local} received their handshake {remote}");
            peer_handshake = their_handshake.clone();

            if !their_handshake.validate(&our_handshake) {
                return Err(Error::HandshakeInvalid);
            }
        } else {
            warn!("{remote} did not send a handshake");
            return Err(Error::HandshakeInvalid);
        }

        let reserved = Reserved::from(peer_handshake.reserved);
        let mut core_socket;

        // if inbound, he have already received their first handshake,
        // send our second handshake here.
        if self.direction.unwrap() == Direction::Inbound {
            debug!("{local} sending the second handshake to {remote}");

            socket.send(our_handshake.clone()).await?;

            let old_parts = socket.into_parts();
            let mut new_parts = FramedParts::new(old_parts.io, MessageCodec);
            new_parts.read_buf = old_parts.read_buf;
            new_parts.write_buf = old_parts.write_buf;
            core_socket = Framed::from_parts(new_parts);

            if reserved[43] {
                // self.ext.push(Codec::ExtendedCodec(ExtendedCodec));
                debug!("{local} sending extended handshake to {remote}");

                // we need to have the info downloaded in order to send the
                // extended message, because it contains the metadata_size
                let info = torrent_ctx.info.read().await;
                let metadata_size =
                    info.to_bencode().ok().map(|v| v.len() as u32);
                drop(info);

                let ext = Extension::supported(metadata_size);

                let msg = Extended::from(ext);
                // let msg: ExtendedMessage = msg.into_extended_message();
                // let msg: Core = msg.into();
                // let msg: Message = msg.into();

                core_socket.send(msg.into()).await?;
            }
        } else {
            let old_parts = socket.into_parts();
            let mut new_parts = FramedParts::new(old_parts.io, MessageCodec);
            new_parts.read_buf = old_parts.read_buf;
            new_parts.write_buf = old_parts.write_buf;
            core_socket = Framed::from_parts(new_parts);
        }

        let handshaking_peer = HandshakingPeer {
            peer_id: peer_handshake.peer_id.into(),
            reserved: peer_handshake.reserved.into(),
            direction: self.direction.unwrap(),
            torrent_ctx,
        };

        Ok(HandshakedPeer { socket: core_socket, peer: handshaking_peer })
    }
}

#[derive(Debug)]
pub struct HandshakingPeer {
    pub peer_id: PeerId,
    pub reserved: BitArray<[u8; 8], Msb0>,
    pub torrent_ctx: Arc<TorrentCtx>,
    pub direction: Direction,
}

/// Peer has succesfuly handshaked and is ready to become a Peer
#[derive(Debug)]
pub struct HandshakedPeer {
    pub(crate) peer: HandshakingPeer,
    pub(crate) socket: Framed<TcpStream, MessageCodec>,
}

impl HandshakingPeer {}
