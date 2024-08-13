//! Types for the metadata protocol codec.

use crate::{
    error::Error, extensions::core::Message, peer::Peer, torrent::TorrentMsg,
};
use bendy::encoding::ToBencode;
use futures::SinkExt;
use tokio::sync::oneshot;
use tokio_util::codec::{Decoder, Encoder};
use tracing::{debug, info};

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

#[derive(Debug)]
pub struct MetadataCodec;

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
            let ext_id = <MetadataCodec as ExtensionTrait>::ID;

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

impl ExtensionTrait for MetadataCodec {
    type Codec = MetadataCodec;
    type Msg = Metadata;

    const ID: u8 = 3;

    async fn handle_msg<T: SinkExt<Message> + Sized + std::marker::Unpin>(
        &self,
        msg: Self::Msg,
        peer: &mut Peer,
        sink: &mut T,
    ) -> Result<(), Error> {
        match &msg {
            Metadata::Response { metadata, payload } => {
                debug!(
                    "{} metadata res from {}",
                    peer.ctx.local_addr, peer.ctx.remote_addr
                );
                debug!("{metadata:?}");

                let peer_ext_id = peer.extension.metadata_size.unwrap();

                peer.torrent_ctx
                    .tx
                    .send(TorrentMsg::DownloadedInfoPiece(
                        peer_ext_id,
                        metadata.piece,
                        payload.clone(),
                    ))
                    .await?;
                peer.torrent_ctx
                    .tx
                    .send(TorrentMsg::SendCancelMetadata {
                        from: peer.ctx.id,
                        index: metadata.piece,
                    })
                    .await?;
            }
            Metadata::Request(piece) => {
                debug!(
                    "{} metadata req from {}",
                    peer.ctx.local_addr, peer.ctx.remote_addr
                );
                debug!("piece = {piece:?}");

                let (tx, rx) = oneshot::channel();
                peer.torrent_ctx
                    .tx
                    .send(TorrentMsg::RequestInfoPiece(*piece, tx))
                    .await?;

                match rx.await? {
                    Some(info_slice) => {
                        info!("sending data with piece {:?}", piece);
                        let payload = MetadataDict::data(*piece, &info_slice)?;
                        sink.send(Core::Extended(Self::ID, payload).into())
                            .await;
                    }
                    None => {
                        info!("sending reject");
                        let r = MetadataDict::reject(*piece)
                            .to_bencode()
                            .map_err(|_| Error::BencodeError)?;
                        sink.send(Core::Extended(Self::ID, r).into()).await;
                    }
                }
            }
            Metadata::Reject(piece) => {
                debug!(
                    "{} metadata res from {}",
                    peer.ctx.local_addr, peer.ctx.remote_addr
                );
                debug!("piece = {piece:?}");
            }
        }
        Ok(())
    }

    fn is_supported(
        &self,
        extension: &crate::extensions::extended::Extension,
    ) -> bool {
        extension.m.ut_metadata.is_some()
    }

    fn codec(&self) -> Self::Codec {
        MetadataCodec
    }
}
