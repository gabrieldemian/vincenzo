use std::sync::Arc;

use crate::{
    daemon::DaemonCtx,
    error::Error,
    extensions::{Core, ExtendedMessage, Extension},
    peer::Peer,
};

use bytes::{Buf, BytesMut};
use tokio_util::codec::{Decoder, Encoder};

pub trait MsgTrait {
    const ID: u8;
}
pub trait ExtDataTrait {}

pub trait ExtensionMsgHandler<Msg: MsgTrait, Data: ExtDataTrait> {
    fn handle_msg(&self, peer: &mut Peer, msg: &Msg, data: &mut Data) -> u8;
}

pub trait ExtensionTrait {}

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

// --- BLANKET IMPLS ---

// Extension can convert all the messages from extension protocols into an
// extended message can be converted to "Core::Extended(extended_message)".
impl<M> TryIntoExtendedMessage<M> for Extension
where
    M: Into<BytesMut> + MsgTrait,
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

impl<T, M> CodecTrait<M> for T where
    T: Encoder<M, Error = Error> + Decoder<Error = Error, Item = M>
{
}
