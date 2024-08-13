use crate::extensions::{
    core::{Core, CoreCodec},
    extended::codec::ExtendedCodec,
    metadata::codec::MetadataCodec,
};

/// From a list of types that implement
/// [`crate::extensions::extended::ExtensionTrait`], generate a [`Message`]
/// enum with all of their messages. Aditionally, a struct `MessageCodec` that
/// implements Encoder and Decoder for it's branches.
///
/// example:
///
/// ```ignore
/// declare_message!(CoreCodec, ExtendedCodec, MetadataCodec);
///
/// pub enum Message {
///     CoreCodec(Core),
///     ExtendedCodec(Extended),
///     MetadataCodec(Metadata),
/// }
/// ```
#[macro_export]
macro_rules! declare_message {
    ( $($codec: tt),* ) => {
        use tokio_util::codec::{Decoder, Encoder};
        use crate::extensions::extended::ExtensionTrait;

        #[derive(Debug)]
        pub struct MessageCodec;

        /// A network message exchanged between Peers, each branch represents a possible
        /// protocol that the message may be.
        #[derive(Debug, Clone, PartialEq)]
        pub enum Message {
            $(
                $codec(<$codec as ExtensionTrait>::Msg),
            )*
        }

        impl Encoder<Message> for MessageCodec {
            type Error = crate::error::Error;

            fn encode(
                &mut self,
                item: Message,
                dst: &mut bytes::BytesMut,
            ) -> Result<(), Self::Error> {
                match item {
                    $(
                        Message::$codec(v) => {
                            $codec.encode(v, dst)?;
                        },
                    )*
                };
                Ok(())
            }
        }

        impl Decoder for MessageCodec {
            type Error = crate::error::Error;
            type Item = Message;

            fn decode(
                &mut self,
                src: &mut bytes::BytesMut,
            ) -> Result<Option<Self::Item>, Self::Error> {
                let core = CoreCodec.decode(src)?;
                // todo: change this error
                let core = core.ok_or(crate::error::Error::PeerIdInvalid)?;
                match core {
                    $(
                        Core::Extended(id, _payload) if id == <$codec as ExtensionTrait>::ID => {
                            let v = $codec.codec().decode(src)?
                                .ok_or(crate::error::Error::PeerIdInvalid)?;
                            return Ok(Some(Message::$codec(v)));
                        },
                    )*
                    _ => Ok(Some(Message::CoreCodec(core)))
                }
            }
        }
    };
}

declare_message!(CoreCodec, ExtendedCodec, MetadataCodec);

#[cfg(test)]
mod tests {
    use crate::extensions::core::CoreId;

    use super::*;

    #[test]
    fn declare_message_works() {
        declare_message!(CoreCodec, MetadataCodec);

        let c = Core::Interested;
        let id = CoreId::Interested as u8;
        let m = Message::CoreCodec(c);

        let mut buff = bytes::BytesMut::new();
        MessageCodec.encode(m, &mut buff).unwrap();

        println!("{:?}", buff.to_vec());

        assert_eq!(buff.to_vec(), vec![0, 0, 0, 1, id]);
    }
}
