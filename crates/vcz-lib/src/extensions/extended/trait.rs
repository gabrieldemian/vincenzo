//! Types for extensions of the peer protocol.

use crate::extensions::ExtendedMessage;
use std::future::Future;

/// Messages of the extension, usually an enum.
/// The ID const is the local peer's. The IDs of the remote peers are shared on
/// the [`Extension`] struct, under the "M" dict and they are different from
/// client to client.
pub trait ExtMsg: TryFrom<ExtendedMessage> {
    const ID: u8;
}

/// Message handler trait for an extension.
///
/// Extensions will implement this trait for `Peer`.
pub trait ExtMsgHandler<Msg>
where
    Msg: ExtMsg,
    Self::Error: From<<Msg as TryFrom<ExtendedMessage>>::Error>,
{
    type Error: std::error::Error;

    fn handle_msg(
        &mut self,
        msg: Msg,
    ) -> impl Future<Output = Result<(), Self::Error>>;
}
