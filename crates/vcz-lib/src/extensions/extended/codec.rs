//! Types for the Extended protocol codec.
//! BEP 10 https://www.bittorrent.org/beps/bep_0010.html

use std::sync::atomic::Ordering;

use super::Extension;
use crate::{
    error::Error,
    extensions::{ExtMsg, ExtMsgHandler, ExtendedMessage},
    peer::{self, Direction, Peer},
};
use bendy::encoding::ToBencode;

impl ExtMsgHandler<Extension> for Peer<peer::Connected> {
    type Error = Error;

    async fn handle_msg(&mut self, msg: Extension) -> Result<(), Self::Error> {
        // send ours extended msg if outbound
        if self.state.ctx.direction == Direction::Outbound {
            let metadata_size = self
                .state
                .ctx
                .torrent_ctx
                .metadata_size
                .load(Ordering::Relaxed);
            let metadata_size =
                if metadata_size == 0 { None } else { Some(metadata_size) };
            let ext = Extension::supported(metadata_size).to_bencode()?;
            self.feed(ExtendedMessage(Extension::ID, ext).into()).await?;
        }

        self.handle_ext(msg).await?;

        Ok(())
    }
}
