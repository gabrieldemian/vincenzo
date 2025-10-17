//! Types for the Extended protocol codec.
//! BEP 10 https://www.bittorrent.org/beps/bep_0010.html

use super::Extension;
use crate::{
    error::Error,
    extensions::{ExtMsg, ExtMsgHandler, ExtendedMessage},
    peer::{self, Direction, Peer},
    torrent::TorrentMsg,
};
use bendy::encoding::ToBencode;
use tokio::sync::oneshot;
use tracing::{debug, trace};

impl ExtMsgHandler<Extension> for Peer<peer::Connected> {
    type Error = Error;

    async fn handle_msg(&mut self, msg: Extension) -> Result<(), Self::Error> {
        debug!("{msg:?}");

        // send ours extended msg if outbound
        if self.state.ctx.direction == Direction::Outbound {
            let metadata_size = {
                let (otx, orx) = oneshot::channel();
                self.state
                    .ctx
                    .torrent_ctx
                    .tx
                    .send(TorrentMsg::GetMetadataSize(otx))
                    .await?;
                orx.await?
            };

            let ext = Extension::supported(metadata_size).to_bencode()?;

            trace!("sending my extended handshake {:?}", ext);
            self.feed(ExtendedMessage(Extension::ID, ext).into()).await?;
        }

        self.handle_ext(msg).await?;

        Ok(())
    }
}
