use std::sync::Arc;

use tokio_util::codec::{Decoder, Encoder};

use crate::peer::PeerCtx;

/// All extensions must implement this trait, usually implemented on Codecs.
pub trait ExtensionTrait<Msg>: Encoder<Msg> + Decoder {
    const ID: u8;

    fn handle_msg(&self, msg: Msg, peer_ctx: Arc<PeerCtx>);
}
