use crate::{error::Error, extensions::Message};

use tokio_util::codec::{Decoder, Encoder};

use crate::extensions::core::Core;

pub trait MessageTrait: TryInto<Core> {
    /// Return the Codec for Self, which is a Message type.
    fn codec(&self) -> impl CodecTrait;
}

pub trait CodecTrait:
    Encoder<Message, Error = Error> + Decoder<Item = Message, Error = Error>
{
}

impl<T> CodecTrait for T where
    T: Encoder<Message, Error = Error> + Decoder<Error = Error, Item = Message>
{
}

pub trait ExtensionTrait2 {
    type Msg: MessageTrait;

    fn codec(&self) -> Box<dyn CodecTrait>;
    fn id(&self) -> u8;
}

// pub trait HandleMsg<T>: ExtensionTrait2<Msg = T> {
//     fn handle_msg(
//         &self,
//         msg: &T,
//         peer: &mut Peer,
//     ) -> impl Future<Output = Result<(), Error>> + Send + Sync;
// }

// All extensions from the extended protocol (Bep 0010) must implement this
// trait.
// pub trait ExtensionTrait: Clone {
//     /// The Message of the extension must know how to convert itself to a
//     /// [`Core::Extended`]
//     type Msg;
//
//     /// Codec for [`Self::Msg`]
//     type Codec: Encoder<Self::Msg> + Decoder + Clone;
//
//     /// The ID of this extension.
//     const ID: u8;
//
//     fn codec(&self) -> Self::Codec;
//
//     /// Given an Extension dict return a boolean if the extension "Self" is
//     /// supported or not.
//     fn is_supported(&self, extension: &Extension) -> bool;
//
//     fn handle_msg(
//         &self,
//         msg: &Self::Msg,
//         peer: &mut Peer,
//     ) -> impl Future<Output = Result<(), Error>> + Send + Sync;
// }
