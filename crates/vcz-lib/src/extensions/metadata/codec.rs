//! Types for the metadata protocol codec.

use super::{Metadata, MetadataMsgType};
use crate::{
    error::Error,
    extensions::{ExtMsgHandler, ExtendedMessage, MetadataPiece},
    peer::{self, Peer},
    torrent::TorrentMsg,
};
use bendy::encoding::ToBencode;
use futures::SinkExt;
use tokio::sync::oneshot;
use tracing::{debug, warn};

impl ExtMsgHandler<Metadata> for Peer<peer::Connected> {
    type Error = Error;

    async fn handle_msg(&mut self, msg: Metadata) -> Result<(), Self::Error> {
        let Some(remote_ext_id) =
            self.state.extension.as_ref().and_then(|v| v.m.ut_metadata)
        else {
            warn!(
                "{} received extended msg but the peer doesnt support the \
                 extended protocol or extended metadata protocol {}",
                self.state.ctx.local_addr, self.state.ctx.remote_addr
            );
            return Ok(());
        };

        match msg.msg_type {
            MetadataMsgType::Response => {
                debug!("< metadata res piece {}", msg.piece);

                self.state
                    .req_man_meta
                    .fulfill_request(&MetadataPiece(msg.piece as usize));

                self.state
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

                self.state
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
                            self.state.ctx.local_addr,
                            self.state.ctx.remote_addr,
                            msg.piece
                        );

                        let msg = Metadata::data(msg.piece, &info_slice)?;

                        self.state
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
                            self.state.ctx.local_addr,
                            self.state.ctx.remote_addr
                        );

                        let r = Metadata::reject(msg.piece).to_bencode()?;

                        self.state
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
