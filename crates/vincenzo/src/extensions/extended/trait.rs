use crate::{error::Error, extensions::core::Message};
use std::future::Future;

use futures::{Sink, SinkExt};
use tokio_util::codec::{Decoder, Encoder};

use crate::{extensions::core::Core, peer::Peer};

use super::Extension;

pub trait MessageTrait: TryInto<Core> {
    /// Return the Codec for Self, which is a Message type.
    fn codec(
        &self,
    ) -> impl Encoder<Self, Error = Error> + Decoder + ExtensionTrait<Msg = Self>;
}

/// All extensions from the extended protocol (Bep 0010) must implement this
/// trait.
pub trait ExtensionTrait: Clone {
    /// The Message of the extension must know how to convert itself to a
    /// [`Core::Extended`]
    type Msg: TryInto<Core>;

    /// Codec for [`Self::Msg`]
    type Codec: Encoder<Self::Msg> + Decoder + Clone;

    /// The ID of this extension.
    const ID: u8;

    fn codec(&self) -> Self::Codec;

    /// Given an Extension dict return a boolean if the extension "Self" is
    /// supported or not.
    fn is_supported(&self, extension: &Extension) -> bool;

    fn handle_msg<T>(
        &self,
        msg: &Self::Msg,
        peer: &mut Peer,
        sink: &mut T,
    ) -> impl Future<Output = Result<(), Error>> + Send + Sync
    where
        T: SinkExt<Message>
            + Sized
            + std::marker::Unpin
            + Send
            + Sync
            + Sink<Message, Error = Error>;
}
