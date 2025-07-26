//! Types for the Metadata protocol codec.
//!
//! <http://www.bittorrent.org/beps/bep_0009.html>

mod codec;

// re-exports
pub use codec::*;

use bendy::{
    decoding::{self, Decoder, FromBencode, Object, ResultExt},
    encoding::{Encoder, ToBencode},
};

use crate::extensions::ExtMsg;

use super::super::error;

/// Metadata dict used in the Metadata protocol messages,
/// this dict is used to request, reject, and send data (info).
///
/// # Important
///
/// Since the Metadata codec is handling [`codec::Metadata`] Reject and
/// Request branches with just the [`Metadata::piece`], this is only used in the
/// [`codec::Metadata::Response`] branch.
#[derive(Debug, Clone, PartialEq)]
pub struct Metadata {
    pub msg_type: MetadataMsgType,
    pub piece: u32,
    pub total_size: Option<u32>,
    pub payload: Vec<u8>,
}

impl ExtMsg for Metadata {
    /// This is the ID of the client for the metadata extension.
    const ID: u8 = 3;
}

impl TryInto<Vec<u8>> for Metadata {
    type Error = bendy::encoding::Error;

    fn try_into(self) -> Result<Vec<u8>, Self::Error> {
        self.to_bencode()
    }
}

#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum MetadataMsgType {
    Request = 0,
    Response = 1,
    Reject = 2,
}

impl TryFrom<u8> for MetadataMsgType {
    type Error = error::Error;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        use MetadataMsgType::*;
        match value {
            v if v == Request as u8 => Ok(Request),
            v if v == Response as u8 => Ok(Response),
            v if v == Reject as u8 => Ok(Reject),
            _ => Err(error::Error::BencodeError),
        }
    }
}

impl Metadata {
    pub fn request(piece: u32) -> Self {
        Self {
            msg_type: MetadataMsgType::Request,
            piece,
            total_size: None,
            payload: Vec::new(),
        }
    }

    pub fn data(piece: u32, info: &[u8]) -> Result<Self, error::Error> {
        let metadata = Self {
            msg_type: MetadataMsgType::Response,
            piece,
            total_size: Some(info.len() as u32),
            payload: info.to_vec(),
        };

        Ok(metadata)
    }

    pub fn reject(piece: u32) -> Self {
        Self {
            msg_type: MetadataMsgType::Reject,
            piece,
            total_size: None,
            payload: Vec::new(),
        }
    }
}

impl FromBencode for Metadata {
    // this is never used, just to make the code compile, this fn is required
    // for the `from_bencode` to work.
    fn decode_bencode_object(_object: Object) -> Result<Self, decoding::Error>
    where
        Self: Sized,
    {
        Ok(Self {
            payload: Vec::new(),
            msg_type: MetadataMsgType::Reject,
            piece: 0,
            total_size: None,
        })
    }

    fn from_bencode(bytes: &[u8]) -> Result<Self, decoding::Error>
    where
        Self: Sized,
    {
        let mut msg_type = 0;
        let mut piece = 0;
        let mut total_size = None;
        let mut payload: Vec<u8> = Vec::new();

        for (i, byte) in bytes.windows(2).enumerate() {
            if byte == [101, 101] {
                payload.extend_from_slice(&bytes[i + 2..]);
            }
        }

        let mut dict_decoder = Decoder::new(bytes);
        let mut obj =
            dict_decoder.next_object()?.unwrap().try_into_dictionary()?;

        while let Some(pair) = obj.next_pair()? {
            match pair {
                (b"msg_type", value) => {
                    msg_type =
                        u8::decode_bencode_object(value).context("msg_type")?;
                }
                (b"piece", value) => {
                    piece =
                        u32::decode_bencode_object(value).context("piece")?;
                }
                (b"total_size", value) => {
                    total_size = u32::decode_bencode_object(value)
                        .context("total_size")
                        .map(Some)?;
                }
                _ => {}
            }
        }

        Ok(Self { msg_type: msg_type.try_into()?, piece, total_size, payload })
    }
}

impl ToBencode for Metadata {
    const MAX_DEPTH: usize = 20;

    // this is never used, just to make the code compile, this fn is required
    // for the `to_bencode` to work.
    fn encode(
        &self,
        _encoder: bendy::encoding::SingleItemEncoder,
    ) -> Result<(), bendy::encoding::Error> {
        Ok(())
    }

    fn to_bencode(&self) -> Result<Vec<u8>, bendy::encoding::Error> {
        let mut encoder = Encoder::new();

        encoder.emit_dict(|mut e| {
            e.emit_pair(b"msg_type", self.msg_type as u8)?;
            e.emit_pair(b"piece", self.piece)?;
            if let Some(total_size) = self.total_size {
                e.emit_pair(b"total_size", total_size)?;
            };
            Ok(())
        })?;

        let mut r = encoder.get_output()?;
        r.extend(self.payload.clone());

        Ok(r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_with_payload() {
        let mut raw = b"d8:msg_typei1e5:piecei2e10:total_sizei34256ee".to_vec();

        let payload = [0, 0, 0, 0, 0, 0, 0, 0];
        raw.extend(payload);

        println!("{:?}", String::from_utf8(raw.clone()));

        let dict = Metadata::from_bencode(&raw).unwrap();
        assert_eq!(dict.piece, 2);
        assert_eq!(dict.msg_type, MetadataMsgType::Response);
        assert_eq!(dict.total_size, Some(34256));
        assert_eq!(dict.payload, payload.to_vec());

        let bytes = dict.to_bencode().unwrap();

        assert_eq!(bytes, raw);
    }
}
