//! Types for extensions of the peer protocol.

use std::sync::Arc;

use crate::{
    daemon::DaemonCtx,
    error::Error,
    extensions::{Core, ExtendedMessage, Extension},
    peer::Peer,
};

use bytes::{Buf, BytesMut};
use tokio_util::codec::{Decoder, Encoder};

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
        peer: &mut Peer,
        msg: &Msg,
        // data: &mut Data
    ) -> u8;
}

/// This trait is not implemented manually, but through a blanket
/// implementation for all messages that are : ExtMsg + Into<BytesMut>.
///
/// When the client sends an extended message, it has to be converted into a
/// Core message first.
///
/// ExtMsg -> ExtendedMessage -> Core::Extended(ExtendedMessage)
pub trait TryIntoExtendedMessage<Msg>
where
    Msg: Into<BytesMut> + ExtMsg,
{
    type Error;
    fn try_into_extended_msg(
        &self,
        msg: Msg,
    ) -> Result<ExtendedMessage, Self::Error>;
}

// --- BLANKET IMPLS ---

impl<M> TryIntoExtendedMessage<M> for Extension
where
    M: Into<BytesMut> + ExtMsg,
{
    type Error = Error;
    fn try_into_extended_msg(
        &self,
        msg: M,
    ) -> Result<ExtendedMessage, Self::Error> {
        let bytes: BytesMut = msg.into();
        Ok(ExtendedMessage(M::ID, bytes.to_vec()))
    }
}
