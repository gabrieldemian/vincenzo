//! Types for the metadata protocol codec.

use crate::{
    error::Error,
    extensions::{ExtData, ExtMsg, ExtMsgHandler, ExtendedMessage},
    peer::{self, MsgHandler},
    torrent::TorrentMsg,
};
use bendy::{decoding::FromBencode, encoding::ToBencode};
use futures::SinkExt;
use tokio::sync::oneshot;
use tracing::{debug, info, warn};

use super::{Metadata, MetadataMsgType};

#[derive(Debug, Clone)]
pub struct MetadataCodec;

#[derive(Clone)]
pub struct MetadataData();

impl ExtData for MetadataData {}

impl TryFrom<ExtendedMessage> for Metadata {
    type Error = Error;

    fn try_from(value: ExtendedMessage) -> Result<Self, Self::Error> {
        if value.0 != Self::ID {
            return Err(Error::PeerIdInvalid);
        }

        let dict = Metadata::from_bencode(&value.1)?;

        Ok(dict)
    }
}

impl ExtMsgHandler<Metadata, MetadataData> for MsgHandler {
    async fn handle_msg(
        &self,
        peer: &mut peer::Peer<peer::Connected>,
        msg: Metadata,
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

        match msg.msg_type {
            MetadataMsgType::Response => {
                debug!("< metadata res piece {}", msg.piece);

                peer.state
                    .outgoing_requests_info_pieces
                    .retain(|&p| p.0 != msg.piece);

                peer.state
                    .ctx
                    .torrent_ctx
                    .tx
                    .send(TorrentMsg::DownloadedInfoPiece(
                        msg.total_size.ok_or(Error::MessageResponse)?,
                        msg.piece,
                        msg.payload,
                    ))
                    .await?;
            }
            MetadataMsgType::Request => {
                debug!("< metadata req");

                let (tx, rx) = oneshot::channel();

                peer.state
                    .ctx
                    .torrent_ctx
                    .tx
                    .send(TorrentMsg::RequestInfoPiece(msg.piece, tx))
                    .await?;

                match rx.await? {
                    Some(info_slice) => {
                        info!("sending data with piece {:?}", msg.piece);
                        debug!(
                            "{} sending data with piece {} {}",
                            peer.state.ctx.local_addr,
                            peer.state.ctx.remote_addr,
                            msg.piece
                        );

                        let msg = Metadata::data(msg.piece, &info_slice)?;

                        peer.state
                            .sink
                            .send(
                                ExtendedMessage(
                                    remote_ext_id,
                                    msg.to_bencode()?,
                                )
                                .into(),
                            )
                            .await?;
                    }
                    None => {
                        debug!(
                            "{} sending reject {}",
                            peer.state.ctx.local_addr,
                            peer.state.ctx.remote_addr
                        );

                        let r = Metadata::reject(msg.piece).to_bencode()?;

                        peer.state
                            .sink
                            .send(ExtendedMessage(remote_ext_id, r).into())
                            .await?;
                    }
                }
            }
            MetadataMsgType::Reject => {
                debug!("metadata rej piece {}", msg.piece);
            }
        }

        Ok(())
    }
}
