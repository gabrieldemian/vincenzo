use vincenzo_macros::declare_message;

use crate::extensions::{CoreExt, ExtendedExt, ExtensionTrait, MetadataExt};

// declare_message!(CoreExt, ExtendedExt, MetadataExt);
declare_message!(CoreExt, ExtendedExt, MetadataExt);

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_util::codec::Encoder;

    #[test]
    fn declare_message_works() {
        // declare_message!(CoreExt, ExtendedExt, MetadataExt);

        // let c = CoreExt;
        // let codec = c.codec();
        // let mut buff = bytes::BytesMut::new();
        // MessageCodec.encode(m, &mut buff).unwrap();
        //
        // println!("{:?}", buff.to_vec());
        //
        // assert_eq!(buff.to_vec(), vec![0, 0, 0, 1, id]);
    }
}
