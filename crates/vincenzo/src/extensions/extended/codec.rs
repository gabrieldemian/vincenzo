//! Types for the Extended protocol codec.

use crate::{
    error::Error,
    extensions::{CoreCodec, MetadataCodec},
    peer::{Direction, Peer},
};
use std::{fmt::Debug, ops::Deref};

use bendy::{decoding::FromBencode, encoding::ToBencode};
use futures::SinkExt;
use tokio_util::codec::{Decoder, Encoder};
use tracing::debug;
use vincenzo_macros::{Extension, Message};

use crate::extensions::core::Core;

use super::{CodecTrait, Extension, ExtensionTrait2};

/// Extended handshake from the Extended protocol, other extended messages have
/// their own enum type.
#[derive(Debug, Clone, PartialEq, Message)]
pub enum Extended {
    Extension(Extension),
}

impl From<Extension> for Extended {
    fn from(value: Extension) -> Self {
        Self::Extension(value)
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

impl TryInto<Core> for Extended {
    type Error = Error;

    /// Try to convert an [`Extended`] message to a [`Core::Extended`] message.
    fn try_into(self) -> Result<Core, Self::Error> {
        let bytes = self.to_bencode().map_err(|_| Error::BencodeError)?;
        Ok(Core::Extended(ExtendedExt.id(), bytes))
    }
}

impl TryInto<Extended> for Core {
    type Error = Error;

    /// Try to convert a [`Core::Extended`] to [`Extended`] message.
    fn try_into(self) -> Result<Extended, Self::Error> {
        let ext_id = ExtendedExt.id();

        if let Core::Extended(id, payload) = self {
            if id != ext_id {
                // todo: change this error
                return Err(crate::error::Error::PeerIdInvalid);
            }
            let ext = Extension::from_bencode(&payload)
                .map_err(|_| Error::BencodeError)?;
            return Ok(Extended::Extension(ext));
        }
        // todo: change this error
        Err(crate::error::Error::PeerIdInvalid)
    }
}

impl Encoder<Message> for ExtendedCodec {
    type Error = crate::error::Error;

    fn encode(
        &mut self,
        item: Message,
        dst: &mut bytes::BytesMut,
    ) -> Result<(), Self::Error> {
        // Core::Extended
        // let core: Core = item.try_into()?;
        CoreCodec.encode(item, dst)
    }
}

impl Decoder for ExtendedCodec {
    type Error = crate::error::Error;
    type Item = Message;

    fn decode(
        &mut self,
        src: &mut bytes::BytesMut,
    ) -> Result<Option<Self::Item>, Self::Error> {
        // Core::Extended
        let core: Option<Message> = CoreCodec.decode(src)?;
        let Some(core) = core else { return Ok(None) };
        let extended: Extended = core.try_into()?;
        let message: Message = extended.into();
        Ok(Some(message))
    }
}

#[derive(Debug, Clone, Extension)]
#[extension(id = 0, codec = ExtendedCodec, msg = Extended)]
pub struct ExtendedExt;

// impl ExtensionTrait for ExtendedCodec {
//     type Codec = ExtendedCodec;
//     type Msg = Extended;
//
//     const ID: u8 = 0;
//
//     async fn handle_msg(
//         &self,
//         msg: &Self::Msg,
//         peer: &mut Peer,
//     ) -> Result<(), Error> {
//         debug!(
//             "{} extended handshake from {}",
//             peer.ctx.local_addr, peer.ctx.remote_addr
//         );
//
//         // todo: maybe make Into<Vec<Codec>> for Extension
//         if msg.0.m.ut_metadata.is_some() {
//             peer.ext.push(Codec::MetadataCodec(MetadataCodec));
//         }
//
//         peer.extension = msg.0.clone();
//
//         if peer.ctx.direction == Direction::Outbound {
//             let metadata_size = peer.extension.metadata_size.unwrap();
//
//             // create our Extension dict, that the local client supports.
//             let ext = Extension::supported(Some(metadata_size))
//                 .to_bencode()
//                 .map_err(|_| Error::BencodeError)?;
//
//             // and send to the remote peer
//             let core = Core::Extended(Self::ID, ext);
//
//             peer.sink.send(core.into()).await?;
//
//             peer.try_request_info().await?;
//         }
//         Ok(())
//     }
//
//     fn is_supported(&self, extension: &Extension) -> bool {
//         extension.v.is_some()
//     }
//
//     fn codec(&self) -> Self::Codec {
//         ExtendedCodec
//     }
// }
