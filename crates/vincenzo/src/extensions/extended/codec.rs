//! Types for the Extended protocol codec.
//! BEP 10 https://www.bittorrent.org/beps/bep_0010.html

use crate::{
    error::Error,
    extensions::{Core, ExtMsg, ExtMsgHandler, ExtendedMessage},
    peer::{self, session::Session, Direction, MsgHandler},
    torrent::TorrentMsg,
};
use std::{fmt::Debug, ops::Deref};

use bendy::{decoding::FromBencode, encoding::ToBencode};
use bytes::BytesMut;
use futures::SinkExt;
use tracing::info;
use vincenzo_macros::Message;

use super::Extension;

/// Extended handshake from the Extended protocol, other extended messages have
/// their own enum type.
#[derive(Debug, Clone, PartialEq, Message)]
pub enum Extended {
    Extension(Extension),
}

impl ExtMsg for Extended {
    /// handshake ID
    const ID: u8 = 0;
}

impl From<Extension> for Extended {
    fn from(value: Extension) -> Self {
        Self::Extension(value)
    }
}

impl TryFrom<ExtendedMessage> for Extended {
    type Error = Error;

    fn try_from(value: ExtendedMessage) -> Result<Self, Self::Error> {
        if value.0 != Self::ID {
            return Err(Error::PeerIdInvalid);
        }

        let extension = Extension::from_bencode(&value.1)?;

        Ok(Extended::Extension(extension))
    }
}

impl Deref for Extended {
    type Target = Extension;
    fn deref(&self) -> &Self::Target {
        match self {
            Self::Extension(v) => v,
        }
    }
}

impl From<Extended> for BytesMut {
    fn from(val: Extended) -> Self {
        let mut dst = BytesMut::new();

        let Extended::Extension(extension) = val;
        let payload = extension.to_bencode().unwrap();
        dst.extend(payload);

        dst
    }
}

impl From<Extended> for Extension {
    fn from(value: Extended) -> Self {
        match value {
            Extended::Extension(ext) => ext,
        }
    }
}

impl TryFrom<Extension> for ExtendedMessage {
    type Error = Error;

    fn try_from(value: Extension) -> Result<Self, Self::Error> {
        let buf: Vec<u8> = value.try_into()?;
        Ok(ExtendedMessage(Extended::ID, buf))
    }
}

impl ExtMsgHandler<Extended, Extension> for MsgHandler {
    async fn handle_msg(
        &self,
        peer: &mut peer::Peer<peer::Connected>,
        msg: Extended,
    ) -> Result<(), Error> {
        tracing::info!(
            "{} extended handshake from {}",
            peer.state.ctx.local_addr,
            peer.state.ctx.remote_addr
        );

        let ext: Extension = msg.into();

        if let Some(meta_size) = ext.metadata_size {
            peer.state
                .torrent_ctx
                .tx
                .send(TorrentMsg::MetadataSize(meta_size))
                .await?;
        }

        tracing::info!("ext of peer {:?}", ext);

        // send ours extended msg if outbound
        if peer.state.ctx.direction == Direction::Outbound {
            let magnet = &peer.state.torrent_ctx.magnet;

            let info = peer.state.torrent_ctx.info.read().await;
            let metadata_size = match info.metadata_size {
                Some(size) => size,
                None => magnet.length().unwrap_or(0),
            };

            let ext = Extension::supported(Some(metadata_size)).to_bencode()?;

            info!(
                "sending my extended handshake {:?}",
                String::from_utf8(ext.clone())
            );
            let core: Core = ExtendedMessage(Extended::ID, ext).into();

            peer.state.sink.send(core).await?;
        }

        // the max number of block_infos to request
        let n = ext.reqq.unwrap_or(Session::DEFAULT_REQUEST_QUEUE_LEN);
        peer.state.session.target_request_queue_len = n;
        peer.state.ext_states.extension = Some(ext);

        Ok(())
    }
}
