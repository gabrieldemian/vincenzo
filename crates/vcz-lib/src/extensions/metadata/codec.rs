//! Types for the metadata protocol codec.

use super::{Metadata, MetadataMsgType};
use crate::{
    error::Error,
    extensions::{ExtMsgHandler, ExtendedMessage, MetadataPiece},
    peer::{self, MsgHandler},
    torrent::TorrentMsg,
};
use bendy::encoding::ToBencode;
use futures::SinkExt;
use tokio::sync::oneshot;
use tracing::{debug, warn};

impl ExtMsgHandler<Metadata> for MsgHandler {
    async fn handle_msg(
        &self,
        peer: &mut peer::Peer<peer::Connected>,
        msg: Metadata,
    ) -> Result<(), Error> {
        let Some(remote_ext_id) =
            peer.state.extension.as_ref().and_then(|v| v.m.ut_metadata)
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
                    .req_man_meta
                    .remove_request(&MetadataPiece(msg.piece as usize));

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
                        debug!("sending data with piece {:?}", msg.piece);
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
                debug!("metadata reject piece {}", msg.piece);
            }
        }

        Ok(())
    }
}
