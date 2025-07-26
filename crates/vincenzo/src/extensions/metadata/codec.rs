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
use tracing::{debug, info, warn};

use super::{Metadata as MetadataDict, MetadataMsgType};

/// Messages of the extended metadata protocol, used to exchange pieces of the
/// `Info` of a metadata file.
#[derive(Debug, Clone, PartialEq)]
pub enum MetadataMsg {
    /// id: 0 Request(piece)
    Request(u32),

    /// id: 1, also named "Data"
    Response { metadata: crate::extensions::Metadata, payload: Vec<u8> },

    /// id: 2, Reject(piece)
    Reject(u32),
}

impl MetadataMsg {
    pub fn from_request(piece: u32) -> Self {
        Self::Request(piece)
    }

    pub fn from_reject(piece: u32) -> Self {
        Self::Reject(piece)
    }

    pub fn from_response(
        metadata: crate::extensions::Metadata,
        payload: Vec<u8>,
    ) -> Self {
        Self::Response { metadata, payload }
    }
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
            .and_then(|v| v.m.ut_metadata)
        else {
            warn!(
                "{} received extended msg but the peer doesnt support the \
                 extended protocol or extended metadata protocol {}",
                peer.state.ctx.local_addr, peer.state.ctx.remote_addr
            );
            return Ok(());
        };

        match msg {
            MetadataMsg::Response { metadata, payload } => {
                info!(
                    "{} metadata res {metadata:?}",
                    peer.state.ctx.remote_addr
                );

                peer.state
                    .torrent_ctx
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

                peer.state
                    .torrent_ctx
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
                info!(
                    "{} metadata rej piece {piece}",
                    peer.state.ctx.remote_addr
                );
            }
        }

        Ok(())
    }
}

/// Encode Metadata without any knowledge of the Core protocol.
impl TryFrom<MetadataMsg> for BytesMut {
    type Error = Error;

    fn try_from(val: MetadataMsg) -> Result<Self, Self::Error> {
        // <metadata_msg_type> <payload>
        let mut buf = BytesMut::new();
        let mut p: Option<_> = None;

        let a = match &val {
            v @ (MetadataMsg::Reject(piece) | MetadataMsg::Request(piece)) => {
                MetadataDict {
                    msg_type: v.msg_type(),
                    total_size: None,
                    piece: *piece,
                }
            }
            MetadataMsg::Response { metadata, payload } => {
                p = Some(payload);
                MetadataDict {
                    msg_type: MetadataMsgType::Response,
                    piece: metadata.piece,
                    total_size: None,
                }
            }
        };

        // buf.put_u8(MetadataMsg::ID);
        buf.extend(a.to_bencode()?);

        if let Some(p) = p {
            buf.extend(p);
        }

        Ok(buf)
    }
}

impl TryFrom<ExtendedMessage> for MetadataMsg {
    type Error = Error;
    fn try_from(value: ExtendedMessage) -> Result<Self, Self::Error> {
        let mut buf = BytesMut::new();
        buf.extend(value.1);
        MetadataCodec.decode(&mut buf)?.ok_or(Error::BencodeError)
    }
}

impl Encoder<MetadataMsg> for MetadataCodec {
    type Error = Error;

    fn encode(
        &mut self,
        item: MetadataMsg,
        dst: &mut BytesMut,
    ) -> Result<(), Self::Error> {
        let b: BytesMut = item.try_into()?;
        dst.extend(b);
        Ok(())
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

#[cfg(test)]
mod tests {
    use bendy::decoding::FromBencode;

    use crate::extensions::{
        extended::TryIntoExtendedMessage, Core, CoreCodec,
    };

    use super::*;

    #[tokio::test]
    async fn metadata_to_core() {
        // let req = MetadataMsg::Request(1);
        // let msg = MetadataMsg::try_into_extended_msg(req).unwrap();
        // println!("ext {msg:?}");
        // println!("{:?}", String::from_utf8(msg.1).unwrap());
        //
        // println!("---");
        //
        // let req = MetadataMsg::Response {
        //     metadata: MetadataDict {
        //         msg_type: MetadataMsgType::Response,
        //         piece: 1,
        //         total_size: Some(987),
        //     },
        //     payload: vec![0, 0, 0, 0, 0],
        // };
        // let msg = MetadataMsg::try_into_extended_msg(req).unwrap();
        //
        // println!("ext {msg:?}");
        // println!("{:?}", String::from_utf8(msg.1).unwrap());

        // let raw = [
        //     100, 56, 58, 109, 115, 103, 95, 116, 121, 112, 101, 105, 49, 101,
        //     53, 58, 112, 105, 101, 99, 101, 105, 49, 101, 101, 0, 0, 0, 0, 0,
        // ];
        // let mut b = BytesMut::new();
        // b.extend(raw);
        // let core = CoreCodec.decode(&mut b).unwrap().unwrap();
        // println!("{core:?}");

        let help = "d8:msg_typei0e5:piecei0ee";

        let msg = MetadataMsg::Request(0);
        let x: BytesMut = msg.clone().try_into().unwrap();

        println!("msg {:#?}", String::from_utf8(x.to_vec()));

        let mut msg = MetadataMsg::try_into_extended_msg(msg).unwrap();

        println!("help {help:#?}");

        let core: Core = msg.into();

        println!("core {core:?}");

        let mut b = BytesMut::new();
        CoreCodec.encode(core.clone(), &mut b).unwrap();

        println!("size u32 {}", b.get_u32());
        println!("id u8 {}", b.get_u8());
        println!("ext id u8 {}", b.get_u8());

        println!("core {b:?}");

        // let net_core = CoreCodec.decode(&mut b).unwrap();
        // println!("new core {b:?}");

        let msg = assert!(false);
    }
}
