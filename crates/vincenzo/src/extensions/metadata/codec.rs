//! Types for the metadata protocol codec.

use crate::{
    error::Error,
    extensions::{ExtData, ExtMsg, ExtMsgHandler, ExtendedMessage},
    peer::{self, MsgHandler},
    torrent::TorrentMsg,
};
use bendy::encoding::ToBencode;
use bytes::{Buf, BufMut, BytesMut};
use futures::SinkExt;
use tokio::sync::oneshot;
use tokio_util::codec::{Decoder, Encoder};
use tracing::{debug, info};

use super::{Metadata as MetadataDict, MetadataMsgType};

/// Messages of the extended metadata protocol, used to exchange pieces of the
/// `Info` of a metadata file.
#[derive(Debug, Clone, PartialEq)]
pub enum MetadataMsg {
    /// id: 0 Request(piece)
    Request(u32),

    /// id: 1, also named "Data"
    Response {
        metadata: crate::extensions::metadata::Metadata,
        payload: Vec<u8>,
    },

    /// id: 2, Reject(piece)
    Reject(u32),
}

#[derive(Clone)]
pub struct MetadataData;

impl ExtData for MetadataData {}

impl ExtMsg for MetadataMsg {
    /// This is the ID of the client for the metadata extension.
    const ID: u8 = 3;
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
impl From<MetadataMsg> for BytesMut {
    fn from(val: MetadataMsg) -> Self {
        // <metadata_msg_type> <payload>
        let mut dst = BytesMut::new();

        let metadata_msg_type = val.msg_type();
        dst.put_u8(metadata_msg_type as u8);

        match val {
            MetadataMsg::Request(piece) | MetadataMsg::Reject(piece) => {
                dst.put_u32(piece);
            }
            MetadataMsg::Response { metadata, payload } => {
                dst.extend(metadata.to_bencode().unwrap());
                dst.extend(payload);
            }
        }

        dst
    }
}

impl Encoder<MetadataMsg> for MetadataCodec {
    type Error = Error;

    fn encode(
        &mut self,
        item: MetadataMsg,
        dst: &mut bytes::BytesMut,
    ) -> Result<(), Self::Error> {
        let mut b: BytesMut = item.into();
        std::mem::swap(&mut b, dst);
        Ok(())
    }
}

impl TryFrom<ExtendedMessage> for MetadataMsg {
    type Error = Error;
    fn try_from(value: ExtendedMessage) -> Result<Self, Self::Error> {
        if value.0 != MetadataMsg::ID {
            return Err(crate::error::Error::PeerIdInvalid);
        }
        let mut buf = BytesMut::new();

        MetadataCodec.decode(&mut buf)?.ok_or(Error::BencodeError)
    }
}

impl Decoder for MetadataCodec {
    type Error = Error;
    type Item = MetadataMsg;

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
                MetadataMsg::Request(src.get_u32())
            }

            MetadataMsgType::Response => {
                let remaining = src.remaining();
                let mut payload = vec![0u8; remaining];
                src.copy_to_slice(&mut payload);

                let (metadata, payload) = MetadataDict::extract(payload)?;

                MetadataMsg::Response { metadata, payload }
            }
        }))
    }
}

impl MetadataMsg {
    pub const fn msg_type(&self) -> MetadataMsgType {
        match self {
            Self::Request(..) => MetadataMsgType::Request,
            Self::Response { .. } => MetadataMsgType::Response,
            Self::Reject(..) => MetadataMsgType::Reject,
        }
    }
}

impl ExtMsgHandler<MetadataMsg, MetadataData> for MsgHandler {
    async fn handle_msg(
        &self,
        peer: &mut peer::Peer<peer::Connected>,
        msg: MetadataMsg,
    ) -> Result<(), Error> {
        let Some(remote_ext_id) = peer
            .state
            .ext_states
            .extension
            .as_ref()
            .and_then(|v| v.m.lt_metadata)
        else {
            debug!(
                "{} received extended msg but the peer doesnt support the \
                 extended protocol or extended metadata protocol {}",
                peer.state.ctx.local_addr, peer.state.ctx.remote_addr
            );
            return Ok(());
        };

        match msg {
            MetadataMsg::Response { metadata, payload } => {
                debug!(
                    "{} metadata res from {}",
                    peer.state.ctx.local_addr, peer.state.ctx.remote_addr
                );
                debug!("{metadata:?}");

                peer.state.torrent_ctx
                    .tx
                    .send(TorrentMsg::DownloadedInfoPiece(
                        metadata.total_size.unwrap_or(u32::MAX),
                        metadata.piece,
                        payload.clone(),
                    ))
                    .await?;
            }
            MetadataMsg::Request(piece) => {
                debug!(
                    "{} metadata req from {}",
                    peer.state.ctx.local_addr, peer.state.ctx.remote_addr
                );
                debug!("piece = {piece:?}");

                let (tx, rx) = oneshot::channel();

                peer.state.torrent_ctx
                    .tx
                    .send(TorrentMsg::RequestInfoPiece(piece, tx))
                    .await?;

                match rx.await? {
                    Some(info_slice) => {
                        info!("sending data with piece {:?}", piece);
                        debug!(
                            "{} sending data with piece {} {piece}",
                            peer.state.ctx.local_addr,
                            peer.state.ctx.remote_addr
                        );

                        let payload = MetadataDict::data(piece, &info_slice)?;

                        peer.state
                            .sink
                            .send(
                                ExtendedMessage(remote_ext_id, payload).into(),
                            )
                            .await?;
                    }
                    None => {
                        debug!(
                            "{} sending reject {}",
                            peer.state.ctx.local_addr,
                            peer.state.ctx.remote_addr
                        );

                        let r = MetadataDict::reject(piece).to_bencode()?;

                        peer.state
                            .sink
                            .send(ExtendedMessage(remote_ext_id, r).into())
                            .await?;
                    }
                }
            }
            MetadataMsg::Reject(piece) => {
                debug!(
                    "{} metadata res from {}",
                    peer.state.ctx.local_addr, peer.state.ctx.remote_addr
                );
                debug!("piece = {piece:?}");
            }
        }

        Ok(())
    }
}
