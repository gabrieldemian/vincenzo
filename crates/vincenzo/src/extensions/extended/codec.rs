//! Types for the Extended protocol codec.
//! BEP 10 https://www.bittorrent.org/beps/bep_0010.html

use crate::{
    daemon::DaemonCtx,
    extensions::{
        ExtDataTrait, ExtendedMessage, ExtensionMsgHandler, MsgConverter,
        MsgTrait, TryIntoExtendedMessage, M,
    },
};
use std::{fmt::Debug, ops::Deref, sync::Arc};

use bendy::{decoding::FromBencode, encoding::ToBencode};
use bytes::BytesMut;
use tokio_util::codec::{Decoder, Encoder};
use vincenzo_macros::{Extension, Message};

use super::{CodecTrait, Extension, ExtensionTraitSecond};

pub trait ExtTrait {}

/// Extended handshake from the Extended protocol, other extended messages have
/// their own enum type.
#[derive(Debug, Clone, PartialEq, Message)]
pub enum Extended {
    Extension(Extension),
}

impl MsgTrait for Extended {
    const ID: u8 = 0;
}

// convert all extended messages (that are not core)
// into an ExtendedMessage which can be converted to core
// like Core::Extended(extended_message)
//
// todo: make this into a blanked impl - done
// impl TryIntoExtendedMessage<Extended> for Extension {
//     type Error = crate::error::Error;
//     fn try_into_extended_msg(
//         &self,
//         msg: Extended,
//     ) -> Result<ExtendedMessage, Self::Error> {
//         let mut bytes: BytesMut = msg.into();
//         Ok(ExtendedMessage(Extended::ID, bytes.to_vec()))
//     }
// }

impl From<Extension> for Extended {
    fn from(value: Extension) -> Self {
        Self::Extension(value)
    }
}

impl Deref for Extended {
    type Target = Extension;
    fn deref(&self) -> &Self::Target {
        match self {
            Self::Extension(v) => v,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ExtendedCodec;

impl From<Extended> for BytesMut {
    fn from(val: Extended) -> Self {
        let mut dst = BytesMut::new();

        let Extended::Extension(extension) = val;
        let payload = extension.to_bencode().unwrap();
        dst.extend(payload);

        dst
    }
}

impl Encoder<Extended> for ExtendedCodec {
    type Error = crate::error::Error;

    fn encode(
        &mut self,
        item: Extended,
        dst: &mut bytes::BytesMut,
    ) -> Result<(), Self::Error> {
        let mut b: BytesMut = item.into();
        std::mem::swap(&mut b, dst);
        Ok(())
    }
}

impl Decoder for ExtendedCodec {
    type Error = crate::error::Error;
    type Item = Extended;

    fn decode(
        &mut self,
        src: &mut bytes::BytesMut,
    ) -> Result<Option<Self::Item>, Self::Error> {
        let extension = Extension::from_bencode(src)
            .map_err(|_| crate::error::Error::BencodeError)?;
        Ok(Some(Extended::Extension(extension)))
    }
}

// #[derive(Debug, Clone, Extension, Copy)]
// #[derive(Debug, Clone, Copy)]
// #[extension(id = 0, codec = ExtendedCodec, msg = Extended)]
// pub struct ExtendedExt;

// #[allow(dead_code)]
// pub struct ExtendedData {
//     extension: Extension,
// }

// impl ExtDataTrait for ExtendedData {}

// impl ExtensionMsgHandler for ExtendedExt {
//     type Msg = Extended;
//     type Data = ExtendedData;
//     fn handle_msg(&self, msg: &Self::Msg, _data: &mut Self::Data) -> u8 {
//         if msg.m.ut_metadata.is_some() {
//         }
//         2
//     }
// }

// impl ExtensionTrait for ExtendedCodec {
//     type Codec = ExtendedCodec;
//     type Msg = Extended;
//
//     const ID: u8 = 0;
//
//     async fn handle_msg(
//         &self,
//         msg: &Self::Msg,
//         peer: &mut Peer,
//     ) -> Result<(), Error> {
//         debug!(
//             "{} extended handshake from {}",
//             peer.ctx.local_addr, peer.ctx.remote_addr
//         );
//
//         // todo: maybe make Into<Vec<Codec>> for Extension
//         if msg.0.m.ut_metadata.is_some() {
//          // send msg to daemon to update ext and extdata
//             peer.ext.push(Codec::MetadataCodec(MetadataCodec));
//         }
//
//         peer.extension = msg.0.clone();
//
//         if peer.ctx.direction == Direction::Outbound {
//             let metadata_size = peer.extension.metadata_size.unwrap();
//
//             // create our Extension dict, that the local client supports.
//             let ext = Extension::supported(Some(metadata_size))
//                 .to_bencode()
//                 .map_err(|_| Error::BencodeError)?;
//
//             // and send to the remote peer
//             let core = Core::Extended(Self::ID, ext);
//
//             peer.sink.send(core.into()).await?;
//
//             peer.try_request_info().await?;
//         }
//         Ok(())
//     }
//
//     fn is_supported(&self, extension: &Extension) -> bool {
//         extension.v.is_some()
//     }
//
//     fn codec(&self) -> Self::Codec {
//         ExtendedCodec
//     }
// }
