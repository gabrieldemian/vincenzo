use std::sync::Arc;

use tokio_util::codec::{Decoder, Encoder};

use crate::{extensions::core::Core, peer::PeerCtx};

/// All extensions must implement this trait, usually implemented on Codecs.
pub trait ExtensionTrait {
    /// The Message of the extension must know how to convert itself to a
    /// [`Core::Extended`]
    type Msg: TryInto<Core>;
    type Codec: Encoder<Self::Msg> + Decoder;

    const ID: u8;

    fn handle_msg(&self, msg: Self::Msg, peer_ctx: Arc<PeerCtx>);
    fn codec(&self) -> Self::Codec;
}
