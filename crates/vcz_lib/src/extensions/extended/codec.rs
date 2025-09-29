//! Types for the Extended protocol codec.
//! BEP 10 https://www.bittorrent.org/beps/bep_0010.html

use crate::{
    error::Error,
    extensions::{ExtMsg, ExtMsgHandler, ExtendedMessage},
    peer::{self, Direction, MsgHandler},
    torrent::TorrentMsg,
};
use std::{fmt::Debug, ops::Deref};

use bendy::{decoding::FromBencode, encoding::ToBencode};
use bytes::BytesMut;
use tokio::sync::oneshot;
use tracing::{debug, trace};
use vcz_macros::Message;

use super::Extension;

/// Extended handshake from the Extended protocol, other extended messages have
/// their own enum type.
#[derive(Debug, Clone, PartialEq, Message)]
pub struct Extended(Extension);

impl ExtMsg for Extended {
    /// handshake ID
    const ID: u8 = 0;
}

impl From<Extension> for Extended {
    fn from(value: Extension) -> Self {
        Extended(value)
    }
}

impl TryFrom<ExtendedMessage> for Extended {
    type Error = Error;

    fn try_from(value: ExtendedMessage) -> Result<Self, Self::Error> {
        if value.0 != Self::ID {
            return Err(Error::PeerIdInvalid);
        }

        let extension = Extension::from_bencode(&value.1)?;

        Ok(Extended(extension))
    }
}

impl Deref for Extended {
    type Target = Extension;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl From<Extended> for BytesMut {
    fn from(val: Extended) -> Self {
        let mut dst = BytesMut::new();

        let Extended(extension) = val;
        let payload = extension.to_bencode().unwrap();
        dst.extend(payload);

        dst
    }
}

impl From<Extended> for Extension {
    fn from(value: Extended) -> Self {
        match value {
            Extended(ext) => ext,
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
        let ext: Extension = msg.into();
        debug!("{ext:?}");

        // send ours extended msg if outbound
        if peer.state.ctx.direction == Direction::Outbound {
            let metadata_size = {
                let (otx, orx) = oneshot::channel();
                peer.state
                    .ctx
                    .torrent_ctx
                    .tx
                    .send(TorrentMsg::GetMetadataSize(otx))
                    .await?;
                orx.await?
            };

            let ext = Extension::supported(metadata_size).to_bencode()?;

            trace!("sending my extended handshake {:?}", ext);
            peer.feed(ExtendedMessage(Extended::ID, ext).into()).await?;
        }

        peer.handle_ext(ext).await?;

        Ok(())
    }
}
