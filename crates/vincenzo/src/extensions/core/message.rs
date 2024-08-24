use futures::{Sink, SinkExt};

use crate::{
    extensions::{
        core::{Core, CoreCodec},
        extended::{codec::ExtendedCodec, ExtensionTrait2},
        metadata::codec::MetadataCodec,
    },
    peer::Peer,
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
        use crate::extensions::extended::{ExtensionTrait, MessageTrait};
        use crate::error::Error;

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

        /// A peer supports all the variants of this enum. Used as an enum to allow passing this as
        /// an associated type in other traits. Since different trait bounds in the same trait is
        /// considered to be a new type.
        #[derive(Debug, Clone)]
        pub enum Codec {
            $(
                $codec($codec),
            )*
        }

        $(
            impl From<<$codec as ExtensionTrait>::Msg> for Message {
                fn from(value: <$codec as ExtensionTrait>::Msg) -> Self {
                    Message::$codec(value)
                }
            }
        )*

        impl Encoder<Message> for MessageCodec {
            type Error = Error;

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
            type Error = Error;
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
                        // find if there is an extension that supports the given message extension
                        // ID (src) by comparing their ids.
                        Core::Extended(id, _payload) if id == <$codec as ExtensionTrait>::ID => {
                            let v = $codec.codec().decode(src)?
                                .ok_or(crate::error::Error::PeerIdInvalid)?;
                            return Ok(Some(Message::$codec(v)));
                        },
                    )*
                    // if not, its a Core message
                    _ => Ok(Some(Message::CoreCodec(core)))
                }
            }
        }

        $(
            impl MessageTrait for <$codec as ExtensionTrait>::Msg {
                fn codec(&self) -> impl Encoder<Self, Error = Error> + Decoder + ExtensionTrait<Msg = <$codec as ExtensionTrait>::Msg>
                {
                    $codec
                }

                fn id(&self) -> u8 {
                    <$codec as ExtensionTrait>::ID
                }
            }
        )*

        impl Message {
            pub async fn handle_msg(
                &self,
                peer: &mut Peer,
            ) -> Result<(), Error>
                {
                match self {
                    $(
                        Message::$codec(msg) => {
                            let codec = msg.codec();
                            if codec.is_supported(&peer.extension) {
                                codec.handle_msg(&msg, peer).await?;
                            }
                        }
                    )*
                }
                Ok(())
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
        // use super::{Core, CoreCodec, MetadataCodec};

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
