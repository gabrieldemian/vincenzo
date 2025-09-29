//! Types for extensions of the peer protocol.

use std::future::Future;

use crate::{
    error::Error,
    extensions::ExtendedMessage,
    peer::{self, Peer},
};

use bytes::BytesMut;

/// Data that the extension adds to the peer, which is mutated by it's messages.
pub trait ExtData {}

/// Messages of the extension, usually an enum.
/// The ID const is the local peer's. The IDs of the remote peers are shared on
/// the [`Extension`] struct, under the "M" dict and they are different from
/// client to client.
pub trait ExtMsg {
    const ID: u8;
}

pub trait ExtMsgHandler<Msg: ExtMsg, Data: ExtData> {
    fn handle_msg(
        &self,
        peer: &mut Peer<peer::Connected>,
        msg: Msg,
    ) -> impl Future<Output = Result<(), Error>>;
}

/// This trait is not implemented manually, but through a blanket
/// implementation for all messages that are : ExtMsg + Into<BytesMut>.
///
/// When the client sends an extended message, it has to be converted into a
/// Core message first.
///
/// ExtMsg -> ExtendedMessage -> Core::Extended(ExtendedMessage)
pub trait TryIntoExtendedMessage<M>
where
    M: TryInto<BytesMut> + ExtMsg,
{
    type Error;

    fn try_into_extended_msg(msg: M) -> Result<ExtendedMessage, Self::Error>;
}

// --- BLANKET IMPLS ---

impl<M> TryIntoExtendedMessage<M> for M
where
    M: TryInto<BytesMut> + ExtMsg,
    crate::error::Error: From<<M as TryInto<BytesMut>>::Error>,
{
    type Error = Error;
    fn try_into_extended_msg(msg: M) -> Result<ExtendedMessage, Self::Error> {
        let bytes: BytesMut = msg.try_into()?;
        Ok(ExtendedMessage(M::ID, bytes.into()))
    }
}
