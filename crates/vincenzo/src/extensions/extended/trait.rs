use std::sync::Arc;

use crate::{
    daemon::DaemonCtx,
    error::Error,
    extensions::{Core, ExtendedMessage},
};

use tokio_util::codec::{Decoder, Encoder};

pub trait MessageTrait {
    fn extension(&self) -> impl ExtensionTrait;
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

pub trait ExtensionMsgHandler: ExtensionTrait {
    fn handle_msg(
        &self,
        msg: &Self::Msg,
        data: &mut dyn ExtDataTrait,
        daemon_ctx: Arc<DaemonCtx>,
    ) -> u8;

    // fn handle_msg(
    //     &self,
    // ) -> impl FnMut(&Self::Msg, &mut dyn ExtDataTrait, Arc<DaemonCtx>) -> u8;
}

pub trait ExtensionTrait {
    type Msg: TryInto<ExtendedMessage>;
    // type Data: ExtDataTrait;

    fn id(&self) -> u8;
    fn codec(&self) -> Box<dyn CodecTrait<Self::Msg>>;
    // fn codec(&self) -> impl Encoder<Self::Msg, Error = Error>
    //        + Decoder<Item = Self::Msg, Error = Error>;
}

pub trait ExtDataTrait {}

// 1. Make blanket impl so that all messages implement: `IntoExtendedMessage`
// 2. Another blanket impl so that ExtendedMessage can try into all messages:
//    `TryIntoMessage`

// --- BLANKET IMPLS ---

// impl<M> TryInto<Core> for M
// where
//     M: Into<BytesMut> + MessageTrait,
// {
//     type Error = Error;
//     fn try_into(self) -> Result<Core, Self::Error> {
//         match self {
//             ext => {
//                 let mut bytes: BytesMut = ext.into();
//                 let id = bytes.get_u8();
//                 let mut payload = vec![0u8; bytes.remaining()];
//                 payload.extend(bytes);
//                 Ok(ExtendedMessage(id, payload).into())
//             }
//         }
//     }
// }

// 1
// impl<M> IntoExtendedMessage for M
// where
//     M: Into<BytesMut> + MessageTrait,
// {
//     fn into_extended_message(self) -> ExtendedMessage {
//         let ext = M::extension(&self);
//         let ext_id = ext.id();
//         let payload: BytesMut = self.into();
//         ExtendedMessage(ext_id, vec![])
//     }
// }

// 2
// impl<Ext> TryIntoMessage<Ext> for ExtendedMessage
// where
//     Ext: ExtensionTrait,
// {
//     fn try_into_message(self) -> Result<<Ext>::Msg, Error> {
//         let buff: Vec<u8> = self.into();
//         let mut dst: BytesMut = BytesMut::with_capacity(buff.len());
//         dst.extend(buff);
//         let mut codec = Ext::codec(&self);
//         codec.decode(&mut dst)?.ok_or(Error::BencodeError)
//     }
// }

impl<T, M> CodecTrait<M> for T where
    T: Encoder<M, Error = Error> + Decoder<Error = Error, Item = M>
{
}

// Peer:
// checking for ctx
// they need access to data of Extended
// using sink
// - UpdateExtension
// - SendToSink
//
// Daemon:
// - UpdateExt
// - UpdateExtData
//
// another idea is to have Peer annotated with a macro
// extensions will now hold data
//
// #[derive(Extension)]
// #[extensions(ExtendedExt, MetadataExt)]
// struct Peer
//
// and create the fields of the extensions using the macro
//
// struct Peer {
//   extended: Arc<Extended {..}>,
//   metadata: Arc<Metadata {..}>,
//   handler: MsgHandler,
//   ctx...
// }
//
// a handler that implements a ext data trait for each extension data,
// meaning it can return an arc to the data that is now on Peer
//
// <MsgHandler as Extended>::data();
//
// and with that, extensions are able to declare dependencies and also get
// access to their data.
//
// and no need to do anything on daemon or any trait object.
// but how to handle the messages?
