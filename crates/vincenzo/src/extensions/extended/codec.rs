//! Types for the Extended protocol codec.
//! BEP 10 https://www.bittorrent.org/beps/bep_0010.html

use crate::{
    error::Error,
    extensions::{Core, ExtMsg, ExtMsgHandler, ExtendedMessage},
    peer::{self, Direction, MsgHandler},
};
use std::{fmt::Debug, ops::Deref};

use bendy::{decoding::FromBencode, encoding::ToBencode};
use bytes::BytesMut;
use futures::SinkExt;
use tokio_util::codec::{Decoder, Encoder};
use tracing::debug;
use vincenzo_macros::Message;

use super::Extension;

/// Extended handshake from the Extended protocol, other extended messages have
/// their own enum type.
#[derive(Debug, Clone, PartialEq, Message)]
pub enum Extended {
    Extension(Extension),
}

impl ExtMsg for Extended {
    const ID: u8 = 0;
}

impl From<Extension> for Extended {
    fn from(value: Extension) -> Self {
        Self::Extension(value)
    }
}

impl TryFrom<ExtendedMessage> for Extended {
    type Error = crate::error::Error;
    fn try_from(value: ExtendedMessage) -> Result<Self, Self::Error> {
        if value.0 != Extended::ID {
            return Err(crate::error::Error::PeerIdInvalid);
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

#[derive(Debug, Clone)]
pub struct ExtendedCodec;

impl From<Extended> for BytesMut {
    fn from(val: Extended) -> Self {
        let mut dst = BytesMut::new();

        let Extended::Extension(extension) = val;
        let payload = extension.to_bencode().unwrap();
        dst.extend(payload);

        dst
    }
}

impl Encoder<Extended> for ExtendedCodec {
    type Error = crate::error::Error;

    fn encode(
        &mut self,
        item: Extended,
        dst: &mut bytes::BytesMut,
    ) -> Result<(), Self::Error> {
        let mut b: BytesMut = item.into();
        std::mem::swap(&mut b, dst);
        Ok(())
    }
}

impl Decoder for ExtendedCodec {
    type Error = crate::error::Error;
    type Item = Extended;

    fn decode(
        &mut self,
        src: &mut bytes::BytesMut,
    ) -> Result<Option<Self::Item>, Self::Error> {
        let extension = Extension::from_bencode(src)
            .map_err(|_| crate::error::Error::BencodeError)?;
        Ok(Some(Extended::Extension(extension)))
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
        debug!(
            "{} extended handshake from {}",
            peer.state.ctx.local_addr, peer.state.ctx.remote_addr
        );

        let ext: Extension = msg.into();

        if let Some(lt_metadata) = ext.m.lt_metadata {
            if peer.state.ctx.direction == Direction::Outbound {
                let meta_size = match ext.metadata_size {
                    Some(v) => v,
                    None => {
                        let info = peer.state.torrent_ctx.info.read().await;
                        if info.pieces() != 0 {
                            info.metainfo_size() as u32
                        } else {
                            0
                        }
                    }
                };
                let ext = Extension::supported(Some(meta_size)).to_bencode()?;
                let core: Core = ExtendedMessage(lt_metadata, ext).into();

                peer.state.sink.send(core).await?;
                peer.try_request_info().await?;
            }
        }

        peer.state.ext_states.extension = Some(ext);

        Ok(())
    }
}

// #[derive(Debug, Clone, Extension, Copy)]
// #[extension(id = 0, codec = ExtendedCodec, msg = Extended)]
// pub struct ExtendedExt;
