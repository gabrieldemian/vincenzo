//! Types for the metadata protocol codec.

use crate::error::Error;
use bendy::encoding::ToBencode;
use tokio_util::codec::{Decoder, Encoder};

use crate::extensions::{
    core::{Core, CoreCodec, CoreId},
    extended::ExtensionTrait,
};

use super::{Metadata as MetadataDict, MetadataMsgType};

/// Messages of the extended metadata protocol, used to exchange pieces of the
/// `Info` of a metadata file.
#[derive(Debug, Clone, PartialEq)]
pub enum Metadata {
    /// id: 0
    /// Request(piece)
    Request(u32),
    /// id: 1, also named "Data"
    Response {
        metadata: crate::extensions::metadata::Metadata,
        payload: Vec<u8>,
    },
    /// id: 2
    /// Reject(piece)
    Reject(u32),
}

struct MetadataCodec;

impl Encoder<Metadata> for MetadataCodec {
    type Error = Error;

    fn encode(
        &mut self,
        item: Metadata,
        dst: &mut bytes::BytesMut,
    ) -> Result<(), Self::Error> {
        let item: Core = item.try_into()?;
        CoreCodec.encode(item, dst).map_err(|e| e.into())
    }
}

impl Decoder for MetadataCodec {
    type Error = Error;
    type Item = Metadata;

    fn decode(
        &mut self,
        src: &mut bytes::BytesMut,
    ) -> Result<Option<Self::Item>, Self::Error> {
        let core = CoreCodec.decode(src)?;
        // todo: change this error
        let core = core.ok_or(crate::error::Error::PeerIdInvalid)?;
        let metadata: Metadata = core.try_into()?;
        Ok(Some(metadata))
    }
}

impl TryInto<Metadata> for Core {
    type Error = crate::error::Error;

    /// Parse [`Core::Extended`] into a [`Metadata`] message.
    fn try_into(self) -> Result<Metadata, Self::Error> {
        if let Core::Extended(id, payload) = self {
            let ext_id = <MetadataCodec as ExtensionTrait<Metadata>>::ID;

            if id != ext_id {
                // todo: change this error
                return Err(Error::PeerIdInvalid);
            }

            let (metadata, payload) = MetadataDict::extract(payload)?;

            return Ok(match metadata.msg_type {
                MetadataMsgType::Request => Metadata::Request(metadata.piece),
                MetadataMsgType::Reject => Metadata::Reject(metadata.piece),
                MetadataMsgType::Response => {
                    Metadata::Response { metadata, payload }
                }
            });
        }
        // todo: change this error
        Err(Error::PeerIdInvalid)
    }
}

impl TryInto<Core> for Metadata {
    type Error = Error;

    /// Try to convert a Metadata message to a [`Core::Extended`] message.
    fn try_into(self) -> Result<Core, Self::Error> {
        let id = CoreId::Extended as u8;

        Ok(match self {
            Self::Reject(piece) => Core::Extended(
                id,
                MetadataDict::reject(piece).to_bencode().unwrap(),
            ),
            Self::Request(piece) => Core::Extended(
                id,
                MetadataDict::request(piece).to_bencode().unwrap(),
            ),
            Self::Response { metadata, payload } => {
                let mut buff = metadata.to_bencode().unwrap();
                buff.copy_from_slice(&payload);
                Core::Extended(id, buff)
            }
        })
    }
}

impl ExtensionTrait<Metadata> for MetadataCodec {
    const ID: u8 = 3;

    fn handle_msg(
        &self,
        msg: Metadata,
        peer_ctx: std::sync::Arc<crate::peer::PeerCtx>,
    ) {
    }
}
