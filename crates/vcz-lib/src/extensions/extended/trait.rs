//! Types for extensions of the peer protocol.

use crate::{
    error::Error,
    peer::{self, Peer},
};
use std::future::Future;

/// Messages of the extension, usually an enum.
/// The ID const is the local peer's. The IDs of the remote peers are shared on
/// the [`Extension`] struct, under the "M" dict and they are different from
/// client to client.
pub trait ExtMsg {
    const ID: u8;
}

pub trait ExtMsgHandler<Msg: ExtMsg> {
    fn handle_msg(
        &self,
        peer: &mut Peer<peer::Connected>,
        msg: Msg,
    ) -> impl Future<Output = Result<(), Error>>;
}
