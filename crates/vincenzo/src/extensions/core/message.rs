use vincenzo_macros::declare_message;

use crate::{
    extensions::{
        Core, CoreCodec, ExtendedCodec, ExtensionTrait2, MetadataCodec,
    },
    peer::Peer,
};

declare_message!(CoreCodec, ExtendedCodec, MetadataCodec);

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_util::codec::Encoder;

    #[test]
    fn declare_message_works() {
        // declare_message!(CoreCodec, MetadataCodec);

        let c = Core::Interested;
        let id = CoreId::Interested as u8;
        let m = Message::CoreCodec(c);

        let mut buff = bytes::BytesMut::new();
        MessageCodec.encode(m, &mut buff).unwrap();

        println!("{:?}", buff.to_vec());

        assert_eq!(buff.to_vec(), vec![0, 0, 0, 1, id]);
    }
}
