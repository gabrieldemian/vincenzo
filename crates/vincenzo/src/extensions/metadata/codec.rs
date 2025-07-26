//! Types for the metadata protocol codec.

use crate::{
    error::Error,
    extensions::{ExtData, ExtMsg, ExtMsgHandler, ExtendedMessage},
    peer::{self, MsgHandler},
    torrent::TorrentMsg,
};
use bendy::{decoding::FromBencode, encoding::ToBencode};
use bytes::BytesMut;
use futures::SinkExt;
use tokio::sync::oneshot;
use tracing::{debug, info, warn};

use super::{Metadata as MetadataDict, MetadataMsgType};

#[derive(Debug, Clone)]
pub struct MetadataCodec;

#[derive(Clone)]
pub struct MetadataData(Vec<u8>);

impl ExtData for MetadataData {}

impl ExtMsgHandler<MetadataDict, MetadataData> for MsgHandler {
    async fn handle_msg(
        &self,
        peer: &mut peer::Peer<peer::Connected>,
        msg: MetadataDict,
    ) -> Result<(), Error> {
        info!("received meta msg {msg:?}");

        let Some(remote_ext_id) = peer
            .state
            .ext_states
            .extension
            .as_ref()
            .and_then(|v| v.m.ut_metadata)
        else {
            warn!(
                "{} received extended msg but the peer doesnt support the \
                 extended protocol or extended metadata protocol {}",
                peer.state.ctx.local_addr, peer.state.ctx.remote_addr
            );
            return Ok(());
        };

        // match msg.msg_type {
        //     MetadataMsgType::Response => {
        //         info!("{} metadata res", peer.state.ctx.remote_addr);
        //
        //         let metadata = MetadataDict::extract(buf);
        //
        //         peer.state
        //             .torrent_ctx
        //             .tx
        //             .send(TorrentMsg::DownloadedInfoPiece(
        //                 metadata.total_size.unwrap_or(u32::MAX),
        //                 metadata.piece,
        //                 payload.clone(),
        //             ))
        //             .await?;
        //     }
        //     MetadataMsgType::Request => {
        //         debug!(
        //             "{} metadata req from {}",
        //             peer.state.ctx.local_addr, peer.state.ctx.remote_addr
        //         );
        //         debug!("piece = {piece:?}");
        //
        //         let (tx, rx) = oneshot::channel();
        //
        //         peer.state
        //             .torrent_ctx
        //             .tx
        //             .send(TorrentMsg::RequestInfoPiece(piece, tx))
        //             .await?;
        //
        //         match rx.await? {
        //             Some(info_slice) => {
        //                 info!("sending data with piece {:?}", piece);
        //                 debug!(
        //                     "{} sending data with piece {} {piece}",
        //                     peer.state.ctx.local_addr,
        //                     peer.state.ctx.remote_addr
        //                 );
        //
        //                 let payload = MetadataDict::data(piece,
        // &info_slice)?;
        //
        //                 peer.state
        //                     .sink
        //                     .send(
        //                         ExtendedMessage(remote_ext_id,
        // payload).into(),                     )
        //                     .await?;
        //             }
        //             None => {
        //                 debug!(
        //                     "{} sending reject {}",
        //                     peer.state.ctx.local_addr,
        //                     peer.state.ctx.remote_addr
        //                 );
        //
        //                 let r = MetadataDict::reject(piece).to_bencode()?;
        //
        //                 peer.state
        //                     .sink
        //                     .send(ExtendedMessage(remote_ext_id, r).into())
        //                     .await?;
        //             }
        //         }
        //     }
        //     MetadataMsgType::Reject => {
        //         info!(
        //             "{} metadata rej piece {}",
        //             peer.state.ctx.remote_addr, msg.piece
        //         );
        //     }
        // }

        Ok(())
    }
}

/// Encode Metadata without any knowledge of the Core protocol.
impl TryFrom<MetadataDict> for BytesMut {
    type Error = Error;

    fn try_from(val: MetadataDict) -> Result<Self, Self::Error> {
        // <metadata_msg_type> <payload>
        // let mut buf = BytesMut::new();
        // let mut p: Option<_> = None;

        todo!();

        // let a = match &val {
        //     v @ (MetadataMsg::Reject(piece) | MetadataMsg::Request(piece)) =>
        // {         MetadataDict {
        //             msg_type: v.msg_type(),
        //             total_size: None,
        //             piece: *piece,
        //         }
        //     }
        //     MetadataMsg::Response { metadata, payload } => {
        //         p = Some(payload);
        //         MetadataDict {
        //             msg_type: MetadataMsgType::Response,
        //             piece: metadata.piece,
        //             total_size: None,
        //         }
        //     }
        // };

        // buf.put_u8(MetadataMsg::ID);
        // buf.extend(a.to_bencode()?);

        // if let Some(p) = p {
        //     buf.extend(p);
        // }

        // Ok(buf)
    }
}

impl TryFrom<ExtendedMessage> for MetadataDict {
    type Error = Error;
    fn try_from(value: ExtendedMessage) -> Result<Self, Self::Error> {
        if value.0 != Self::ID {
            return Err(crate::error::Error::PeerIdInvalid);
        }
        let dict = MetadataDict::from_bencode(&value.1)?;
        Ok(dict)
    }
}

// impl Decoder for MetadataCodec {
//     type Error = Error;
//     type Item = MetadataMsg;
//
//     fn decode(
//         &mut self,
//         // <metadata_msg_type> <payload>
//         src: &mut bytes::BytesMut,
//     ) -> Result<Option<Self::Item>, Self::Error> {
//         // minimum 2 bytes, 1 byte for the flag and another one for the
// payload         if src.remaining() < 2 {
//             warn!("hitting this shit {src:?}");
//             return Ok(None);
//         };
//
//         let metadata_msg_type: MetadataMsgType = src.get_u8().try_into()?;
//
//         Ok(Some(match metadata_msg_type {
//             MetadataMsgType::Request | MetadataMsgType::Reject => {
//                 MetadataMsg::Request(src.get_u32())
//             }
//
//             MetadataMsgType::Response => {
//                 let remaining = src.remaining();
//                 let mut payload = vec![0u8; remaining];
//                 src.copy_to_slice(&mut payload);
//
//                 let (metadata, payload) = MetadataDict::extract(payload)?;
//
//                 MetadataMsg::Response { metadata, payload }
//             }
//         }))
//     }
// }
//
// impl Encoder<MetadataMsg> for MetadataCodec {
//     type Error = Error;
//
//     fn encode(
//         &mut self,
//         item: MetadataMsg,
//         dst: &mut BytesMut,
//     ) -> Result<(), Self::Error> {
//         let b: BytesMut = item.try_into()?;
//         dst.extend(b);
//         Ok(())
//     }
// }

// impl MetadataMsg {
//     pub const fn msg_type(&self) -> MetadataMsgType {
//         match self {
//             Self::Request(..) => MetadataMsgType::Request,
//             Self::Response { .. } => MetadataMsgType::Response,
//             Self::Reject(..) => MetadataMsgType::Reject,
//         }
//     }
// }

#[cfg(test)]
mod tests {
    use bendy::decoding::FromBencode;

    use crate::extensions::{
        extended::TryIntoExtendedMessage, Core, CoreCodec,
    };

    use super::*;

    #[tokio::test]
    async fn metadata_to_core() {
        // let raw = [
        //     100, 56, 58, 109, 115, 103, 95, 116, 121, 112, 101, 105, 49, 101,
        //     53, 58, 112, 105, 101, 99, 101, 105, 49, 101, 101, 0, 0, 0, 0, 0,
        // ];

        let msg = assert!(false);
    }
}
