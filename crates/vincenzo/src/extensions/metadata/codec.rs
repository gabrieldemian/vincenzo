//! Types for the metadata protocol codec.

use crate::error::Error;
use bendy::encoding::ToBencode;
use bytes::{Buf, BufMut, BytesMut};
use tokio_util::codec::{Decoder, Encoder};
use vincenzo_macros::{Extension, Message};

use crate::extensions::ExtensionState;

use super::{Metadata as MetadataDict, MetadataMsgType};

/// Messages of the extended metadata protocol, used to exchange pieces of the
/// `Info` of a metadata file.
#[derive(Debug, Clone, PartialEq, Message)]
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

#[derive(Debug, Clone)]
pub struct MetadataCodec;

/// Encode Metadata without any knowledge of the Core protocol, simply put the
/// metadata id and the bytes side by side in a vector.
/// <u8_ext_id><vec>
///
/// The Core protocol can easily transform this into a Core::Extended
///
/// ```ignore
/// ExtendedMessage(ext_id, payload).into()
/// ```
impl From<Metadata> for BytesMut {
    fn from(val: Metadata) -> Self {
        // <metadata_msg_type> <payload>
        let mut dst = BytesMut::new();

        let metadata_msg_type = val.msg_type();
        dst.put_u8(metadata_msg_type as u8);

        match val {
            Metadata::Request(piece) | Metadata::Reject(piece) => {
                dst.put_u32(piece);
            }
            Metadata::Response { metadata, payload } => {
                dst.extend(metadata.to_bencode().unwrap());
                dst.extend(payload);
            }
        }

        dst
    }
}

impl Encoder<Metadata> for MetadataCodec {
    type Error = Error;

    fn encode(
        &mut self,
        item: Metadata,
        dst: &mut bytes::BytesMut,
    ) -> Result<(), Self::Error> {
        let mut b: BytesMut = item.into();
        std::mem::swap(&mut b, dst);
        Ok(())
    }
}

impl Decoder for MetadataCodec {
    type Error = Error;
    type Item = Metadata;

    fn decode(
        &mut self,
        // <metadata_msg_type> <payload>
        src: &mut bytes::BytesMut,
    ) -> Result<Option<Self::Item>, Self::Error> {
        // minimum 2 bytes, 1 byte for the flag and another one for the payload
        if src.remaining() < 2 {
            return Ok(None);
        };

        let meadata_msg_type: MetadataMsgType = src.get_u8().try_into()?;

        Ok(Some(match meadata_msg_type {
            MetadataMsgType::Request | MetadataMsgType::Reject => {
                Metadata::Request(src.get_u32())
            }

            MetadataMsgType::Response => {
                let remaining = src.remaining();
                let mut payload = vec![0u8; remaining];
                src.copy_to_slice(&mut payload);

                let (metadata, payload) = MetadataDict::extract(payload)?;

                Metadata::Response { metadata, payload }
            }
        }))
    }
}

impl Metadata {
    pub const fn msg_type(&self) -> MetadataMsgType {
        match self {
            Self::Request(..) => MetadataMsgType::Request,
            Self::Response { .. } => MetadataMsgType::Response,
            Self::Reject(..) => MetadataMsgType::Reject,
        }
    }
}

// impl TryFrom<Core> for Metadata {
//     type Error = crate::error::Error;
//
//     /// Parse [`Core::Extended`] into a [`Metadata`] message.
//     fn try_from(value: Core) -> Result<Self, Self::Error> {
//         if let Core::Extended(id, payload) = value {
//             let ext_id = MetadataExt.id();
//
//             if id != ext_id {
//                 return Err(Error::PeerIdInvalid);
//             }
//
//             let (metadata, payload) = MetadataDict::extract(payload)?;
//
//             return Ok(match metadata.msg_type {
//                 MetadataMsgType::Request =>
// Metadata::Request(metadata.piece),                 MetadataMsgType::Reject =>
// Metadata::Reject(metadata.piece),                 MetadataMsgType::Response
// => {                     Metadata::Response { metadata, payload }
//                 }
//             });
//         }
//         // todo: change this error
//         Err(Error::PeerIdInvalid)
//     }
// }

// impl TryInto<Core> for Metadata {
//     type Error = Error;
//     /// Try to convert a Metadata message to a [`Core::Extended`] message.
//     fn try_into(self) -> Result<Core, Self::Error> {
//         let id = CoreId::Extended as u8;
//         let ext_msg: ExtendedMessage = self.try_into()?;
//         Ok(match self {
//             Self::Reject(piece) => Core::Extended(
//                 id,
//                 MetadataDict::reject(piece).to_bencode().unwrap(),
//             ),
//             Self::Request(piece) => Core::Extended(
//                 id,
//                 MetadataDict::request(piece).to_bencode().unwrap(),
//             ),
//             Self::Response { metadata, payload } => {
//                 let mut buff = metadata.to_bencode().unwrap();
//                 buff.copy_from_slice(&payload);
//                 Core::Extended(id, buff)
//             }
//         })
//     }
// }

#[derive(Extension, Clone, Copy, Debug)]
#[extension(id = 3, codec = MetadataCodec, msg = Metadata)]
pub struct MetadataExt;

// impl ExtensionTrait for MetadataCodec {
//     async fn handle_msg(
//         &self,
//         msg: &Self::Msg,
//         peer: &mut Peer,
//     ) -> Result<(), Error> {
//         match &msg {
//             Metadata::Response { metadata, payload } => {
//                 debug!(
//                     "{} metadata res from {}",
//                     peer.ctx.local_addr, peer.ctx.remote_addr
//                 );
//                 debug!("{metadata:?}");
//
//                 let peer_ext_id = peer.extension.metadata_size.unwrap();
//
//                 peer.torrent_ctx
//                     .tx
//                     .send(TorrentMsg::DownloadedInfoPiece(
//                         peer_ext_id,
//                         metadata.piece,
//                         payload.clone(),
//                     ))
//                     .await?;
//                 peer.torrent_ctx
//                     .tx
//                     .send(TorrentMsg::SendCancelMetadata {
//                         from: peer.ctx.id.clone(),
//                         index: metadata.piece,
//                     })
//                     .await?;
//             }
//             Metadata::Request(piece) => {
//                 debug!(
//                     "{} metadata req from {}",
//                     peer.ctx.local_addr, peer.ctx.remote_addr
//                 );
//                 debug!("piece = {piece:?}");
//
//                 let (tx, rx) = oneshot::channel();
//                 peer.torrent_ctx
//                     .tx
//                     .send(TorrentMsg::RequestInfoPiece(*piece, tx))
//                     .await?;
//
//                 match rx.await? {
//                     Some(info_slice) => {
//                         info!("sending data with piece {:?}", piece);
//                         let payload = MetadataDict::data(*piece,
// &info_slice)?;                         peer.sink
//                             .send(Core::Extended(Self::ID, payload).into())
//                             .await?;
//                     }
//                     None => {
//                         info!("sending reject");
//                         let r = MetadataDict::reject(*piece)
//                             .to_bencode()
//                             .map_err(|_| Error::BencodeError)?;
//                         peer.sink
//                             .send(Core::Extended(Self::ID, r).into())
//                             .await?;
//                     }
//                 }
//             }
//             Metadata::Reject(piece) => {
//                 debug!(
//                     "{} metadata res from {}",
//                     peer.ctx.local_addr, peer.ctx.remote_addr
//                 );
//                 debug!("piece = {piece:?}");
//             }
//         }
//         Ok(())
//     }
//
//     fn is_supported(
//         &self,
//         extension: &crate::extensions::extended::Extension,
//     ) -> bool {
//         extension.m.ut_metadata.is_some()
//     }
//
//     fn codec(&self) -> Self::Codec {
//         MetadataCodec
//     }
// }
