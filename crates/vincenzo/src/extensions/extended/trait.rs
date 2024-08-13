use crate::{error::Error, extensions::core::Message};
use std::fmt::Debug;

use futures::SinkExt;
use tokio_util::codec::{Decoder, Encoder};

use crate::{extensions::core::Core, peer::Peer};

use super::Extension;

/// All extensions from the extended protocol (Bep 0010) must implement this
/// trait.
pub trait ExtensionTrait: Debug {
    /// The Message of the extension must know how to convert itself to a
    /// [`Core::Extended`]
    type Msg: TryInto<Core>;

    /// Codec for [`Self::Msg`]
    type Codec: Encoder<Self::Msg> + Decoder;

    /// The ID of this extension.
    const ID: u8;

    fn codec(&self) -> Self::Codec;

    /// Given an Extension dict return a boolean if it is supported or not.
    fn is_supported(&self, extension: &Extension) -> bool;

    async fn handle_msg<T: SinkExt<Message> + Sized + std::marker::Unpin>(
        &self,
        msg: Self::Msg,
        peer_ctx: &mut Peer,
        sink: &mut T,
    ) -> Result<(), Error>;
}
