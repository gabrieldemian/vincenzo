//! Types for the Extended protocol codec.

use crate::{
    error::Error,
    extensions::core::{CoreCodec, Message},
    peer::{Direction, Peer},
};
use std::{fmt::Debug, ops::Deref};

use bendy::{decoding::FromBencode, encoding::ToBencode};
use futures::{Sink, SinkExt, Stream};
use tokio_util::codec::{Decoder, Encoder};
use tracing::debug;

use crate::extensions::core::Core;

use super::{Extension, ExtensionTrait};

/// Extended handshake from the Extended protocol, other extended messages have
/// their own enum type.
#[derive(Debug, Clone, PartialEq)]
pub struct Extended(Extension);

impl From<Extension> for Extended {
    fn from(value: Extension) -> Self {
        Self(value)
    }
}

impl Deref for Extended {
    type Target = Extension;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[derive(Debug)]
pub struct ExtendedCodec;

impl TryInto<Core> for Extended {
    type Error = Error;

    /// Try to convert an [`Extended`] message to a [`Core::Extended`] message.
    fn try_into(self) -> Result<Core, Self::Error> {
        let bytes = self.to_bencode().map_err(|_| Error::BencodeError)?;
        Ok(Core::Extended(0, bytes))
    }
}

impl TryInto<Extended> for Core {
    type Error = Error;

    /// Try to convert a [`Core::Extended`] to [`Extended`] message.
    fn try_into(self) -> Result<Extended, Self::Error> {
        let ext_id = <ExtendedCodec as ExtensionTrait>::ID;

        if let Core::Extended(id, payload) = self {
            if id != ext_id {
                // todo: change this error
                return Err(crate::error::Error::PeerIdInvalid);
            }
            let ext = Extension::from_bencode(&payload)
                .map_err(|_| Error::BencodeError)?;
            return Ok(Extended(ext));
        }
        // todo: change this error
        Err(crate::error::Error::PeerIdInvalid)
    }
}

impl Encoder<Extended> for ExtendedCodec {
    type Error = crate::error::Error;

    fn encode(
        &mut self,
        item: Extended,
        dst: &mut bytes::BytesMut,
    ) -> Result<(), Self::Error> {
        // Core::Extended
        let core: Core = item.try_into()?;
        CoreCodec.encode(core, dst).map_err(|e| e.into())
    }
}

impl Decoder for ExtendedCodec {
    type Error = crate::error::Error;
    type Item = Extended;

    fn decode(
        &mut self,
        src: &mut bytes::BytesMut,
    ) -> Result<Option<Self::Item>, Self::Error> {
        let core: Option<Core> = CoreCodec.decode(src)?;
        // todo: change this error
        let core = core.ok_or(Error::PeerIdInvalid)?;
        let extended: Extended = core.try_into()?;
        Ok(Some(extended))
    }
}

impl ExtensionTrait for ExtendedCodec {
    type Codec = ExtendedCodec;
    type Msg = Extended;

    const ID: u8 = 0;

    async fn handle_msg<T: SinkExt<Message> + Sized + std::marker::Unpin>(
        &self,
        msg: Self::Msg,
        peer: &mut Peer,
        sink: &mut T,
    ) -> Result<(), Error> {
        debug!(
            "{} extended handshake from {}",
            peer.ctx.local_addr, peer.ctx.remote_addr
        );
        peer.extension = msg.0;

        if peer.ctx.direction == Direction::Outbound {
            let metadata_size = peer.extension.metadata_size.unwrap();

            // create our Extension dict, that the local client supports.
            let ext = Extension::supported(Some(metadata_size))
                .to_bencode()
                .map_err(|_| Error::BencodeError)?;

            // and send to the remote peer
            let core = Core::Extended(Self::ID, ext);

            let _ = sink.send(core.into()).await;

            // todo: uncomment this once I fix the bounds on this method
            // peer.try_request_info(&mut sink).await?;
        }
        Ok(())
    }

    fn is_supported(&self, extension: &Extension) -> bool {
        extension.v.is_some()
    }

    fn codec(&self) -> Self::Codec {
        ExtendedCodec
    }
}
