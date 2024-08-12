//! Types for the Extended protocol codec.

use crate::{error::Error, extensions::core::CoreCodec};
use std::{fmt::Debug, ops::Deref};

use bendy::{decoding::FromBencode, encoding::ToBencode};
use tokio_util::codec::{Decoder, Encoder};

use crate::extensions::core::Core;

use super::{Extension, ExtensionTrait};

/// Extended handshake from the Extended protocol, other extended messages have
/// their own enum type.
#[derive(Debug, Clone, PartialEq)]
pub struct Extended(Extension);

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
        let ext_id = <ExtendedCodec as ExtensionTrait<Extended>>::ID;

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

impl ExtensionTrait<Extended> for ExtendedCodec {
    // handshake id
    const ID: u8 = 0;

    fn handle_msg(
        &self,
        msg: Extended,
        peer_ctx: std::sync::Arc<crate::peer::PeerCtx>,
    ) {
    }
}
