use std::sync::Arc;

use crate::{
    daemon::DaemonCtx,
    error::Error,
    extensions::{Core, ExtendedMessage, Extension},
};

use bytes::{Buf, BytesMut};
use tokio_util::codec::{Decoder, Encoder};

pub trait MsgTrait {
    const ID: u8;
}
pub trait ExtDataTrait {}

// pub trait ExtensionMsgHandler<Msg: MsgTrait, Data: ExtDataTrait>:
//     ExtensionTrait
// {
//     fn handle_msg(&self, msg: &Msg, data: &mut Data) -> u8;
// }

// pub trait ExtensionMsgHandler: ExtensionTrait {
pub trait ExtensionMsgHandler {
    type Msg: MsgTrait;
    type Data: ExtDataTrait;

    fn handle_msg(&self, msg: &Self::Msg, data: &mut Self::Data) -> u8;
}

pub trait ExtensionTrait {}

/// Convenience trait inherited by ExtendedMessage for all messages. This trait
/// means that ExtendedMessage can try into all messages.
pub trait TryIntoExtendedMessage<Msg>
where
    Msg: Into<BytesMut> + MsgTrait,
{
    type Error;
    fn try_into_extended_msg(
        &self,
        msg: Msg,
    ) -> Result<ExtendedMessage, Self::Error>;
}

pub trait CodecTrait<Msg>:
    Encoder<Msg, Error = Error> + Decoder<Item = Msg, Error = Error>
{
}

// 1. Make blanket impl so that Extension can convert all the extension
//    protocols into a core extended message
// 2. Another blanket impl so that ExtendedMessage can try into all messages:
//    `TryIntoMessage`

// --- BLANKET IMPLS ---

impl<M> TryIntoExtendedMessage<M> for Extension
where
    M: Into<BytesMut> + MsgTrait,
{
    type Error = Error;
    fn try_into_extended_msg(&self, msg: M) -> Result<ExtendedMessage, Self::Error> {
        let bytes: BytesMut = msg.into();
        Ok(ExtendedMessage(M::ID, bytes.to_vec()))
    }
}

impl<T, M> CodecTrait<M> for T where
    T: Encoder<M, Error = Error> + Decoder<Error = Error, Item = M>
{
}
