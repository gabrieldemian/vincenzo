//! Types for the Metadata protocol codec.
//!
//! <http://www.bittorrent.org/beps/bep_0009.html>

mod codec;

// re-exports
pub use codec::*;

use bendy::{
    decoding::{self, FromBencode, Object, ResultExt},
    encoding::ToBencode,
};

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
        Self { msg_type: MetadataMsgType::Request, piece, total_size: None }
    }

    pub fn data(piece: u32, info: &[u8]) -> Result<Vec<u8>, error::Error> {
        let metadata = Self {
            msg_type: MetadataMsgType::Response,
            piece,
            total_size: Some(info.len() as u32),
        };

        let mut bytes = metadata.to_bencode()?;

        bytes.extend_from_slice(info);

        Ok(bytes)
    }

    pub fn reject(piece: u32) -> Self {
        Self { msg_type: MetadataMsgType::Reject, piece, total_size: None }
    }

    /// Tries to extract Info from the given buffer.
    ///
    /// # Errors
    ///
    /// This function will return an error if the buffer is not a valid Data
    /// type of the metadata extension protocol
    pub fn extract(mut buf: Vec<u8>) -> Result<(Self, Vec<u8>), error::Error> {
        let mut metadata_buf = Vec::new();

        // find end of info dict, which is always the first "ee"
        if let Some(i) = buf.windows(2).position(|w| w == b"ee") {
            metadata_buf = buf.drain(..i + 2).collect();
        }

        let metadata = Metadata::from_bencode(&metadata_buf)?;

        Ok((metadata, buf))
    }
}

impl FromBencode for Metadata {
    fn decode_bencode_object(object: Object) -> Result<Self, decoding::Error>
    where
        Self: Sized,
    {
        let mut msg_type = 0;
        let mut piece = 0;
        let mut total_size = None;

        let mut dict_dec = object.try_into_dictionary()?;

        while let Some(pair) = dict_dec.next_pair()? {
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

        // Check that we discovered all necessary fields
        Ok(Self { msg_type: msg_type.try_into()?, piece, total_size })
    }
}

impl ToBencode for Metadata {
    const MAX_DEPTH: usize = 20;

    fn encode(
        &self,
        encoder: bendy::encoding::SingleItemEncoder,
    ) -> Result<(), bendy::encoding::Error> {
        encoder.emit_dict(|mut e| {
            e.emit_pair(b"msg_type", self.msg_type as u8)?;
            e.emit_pair(b"piece", self.piece)?;
            if let Some(total_size) = self.total_size {
                e.emit_pair(b"total_size", total_size)?;
            };
            Ok(())
        })?;
        Ok(())
    }
}
