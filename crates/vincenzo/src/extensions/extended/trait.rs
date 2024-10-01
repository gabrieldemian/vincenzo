use crate::{
    error::Error,
    extensions::{ExtendedMessage, Message},
};

use bytes::BytesMut;
use tokio_util::codec::{Decoder, Encoder};

pub trait MessageTrait {
    fn extension() -> impl ExtensionTrait;
}

/// A message that is able to decode itself in the format required by
/// Core::Extended(ExtendedMessage).
pub trait IntoExtendedMessage {
    fn into_extended_message(self) -> ExtendedMessage;
}

/// Convenience trait inherited by ExtendedMessage for all messages. This trait
/// means that ExtendedMessage can try into all messages.
pub trait TryIntoMessage<Ext> {
    fn try_into_message(self) -> Result<<Ext>::Msg, Error>
    where
        Ext: ExtensionTrait;
}

pub trait CodecTrait<Msg>:
    Encoder<Msg, Error = Error> + Decoder<Item = Msg, Error = Error>
{
}

pub trait ExtensionTrait: Copy + Clone {
    type Msg;
    const ID: u8;
    fn codec() -> impl Encoder<Self::Msg, Error = Error>
           + Decoder<Item = Self::Msg, Error = Error>;
    fn id(&self) -> u8;
}

pub trait ExtDataTrait {
    fn handle_msg(&mut self, msg: &Message) -> Result<(), Error>;
}

pub trait ExtTrait {
    type Msg: TryInto<ExtendedMessage>;
    fn id(&self) -> u8;
    fn handle_msg(&mut self, msg: &Self::Msg) -> Result<(), Error>;
}

// 1. Make blanket impl so that all messages implement: `IntoExtendedMessage`
// 2. Another blanket impl so that ExtendedMessage can try into all messages:
//    `TryIntoMessage`

// --- BLANKET IMPLS ---

// 1
impl<M> IntoExtendedMessage for M
where
    M: Into<BytesMut> + MessageTrait,
{
    fn into_extended_message(self) -> ExtendedMessage {
        let ext = M::extension();
        let ext_id = ext.id();
        let payload: BytesMut = self.into();
        ExtendedMessage(ext_id, payload.to_vec())
    }
}

// 2
impl<Ext> TryIntoMessage<Ext> for ExtendedMessage
where
    Ext: ExtensionTrait,
{
    fn try_into_message(self) -> Result<<Ext>::Msg, Error> {
        let buff: Vec<u8> = self.into();
        let mut dst: BytesMut = BytesMut::with_capacity(buff.len());
        dst.extend(buff);
        let mut codec = Ext::codec();
        codec.decode(&mut dst)?.ok_or(Error::BencodeError)
    }
}

impl<T, M> CodecTrait<M> for T where
    T: Encoder<M, Error = Error> + Decoder<Error = Error, Item = M>
{
}
