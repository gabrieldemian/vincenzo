use bendy::{
    decoding::{FromBencode, Object, ResultExt},
    encoding::{AsString, ToBencode},
};

/// This is the payload of the extension protocol described on:
/// BEP 10 - Extension Protocol
/// http://www.bittorrent.org/beps/bep_0010.html
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Extension {
    /// messages (supported extensions)
    pub m: M,
    /// port
    pub p: Option<u16>,
    /// a string identifying the client and the version
    pub v: Option<String>,
    /// number of outstanding requests messages this client supports
    /// without dropping any.
    pub reqq: Option<u16>,
    /// added by BEP 9
    /// the size of the metadata file, which is the
    /// info-dictionary part of the metainfo(.torrent) file
    pub metadata_size: Option<u32>,
}

/// metadata of the Extension struct
/// lists all extensions that a peer supports
/// in our case, we only support ut_metadata at the moment
/// and naturally, we are only interested in reading this part of the metadata
#[derive(Debug, Clone, Default, PartialEq)]
pub struct M {
    pub ut_metadata: Option<u8>,
    pub ut_pex: Option<u8>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct MetadataMsg {
    pub msg_type: u32,
    pub piece: u32,
    pub total_size: Option<u32>,
    pub data: Option<Vec<u8>>,
}

impl ToBencode for MetadataMsg {
    const MAX_DEPTH: usize = 20;
    fn encode(
        &self,
        encoder: bendy::encoding::SingleItemEncoder,
    ) -> Result<(), bendy::encoding::Error> {
        encoder.emit_dict(|mut e| {
            e.emit_pair(b"msg_type", self.msg_type)?;
            e.emit_pair(b"piece", self.piece)?;
            if let Some(total_size) = self.total_size {
                e.emit_pair(b"total_size", total_size)?;
            }
            Ok(())
        })
    }
}

impl ToBencode for M {
    const MAX_DEPTH: usize = 20;
    fn encode(
        &self,
        encoder: bendy::encoding::SingleItemEncoder,
    ) -> Result<(), bendy::encoding::Error> {
        encoder.emit_dict(|mut e| {
            if let Some(ut_metadata) = self.ut_metadata {
                e.emit_pair(b"ut_metadata", ut_metadata)?;
            }
            if let Some(ut_pex) = self.ut_pex {
                e.emit_pair(b"ut_pex", ut_pex)?;
            }
            Ok(())
        })
    }
}

impl FromBencode for M {
    fn decode_bencode_object(object: Object) -> Result<Self, bendy::decoding::Error>
    where
        Self: Sized,
    {
        // the dict is the very first data structure of the bencoded string
        // a Rust Struct is the equivalent of a dict in bencode
        // and so that makes `Extension` struct the root dict of the bencoded string
        let mut dict = object.try_into_dictionary()?;
        // inside this dict we have other data structures
        let mut ut_metadata = None;
        let mut ut_pex = None;

        while let Some(pair) = dict.next_pair()? {
            match pair {
                (b"ut_metadata", value) => {
                    ut_metadata = u8::decode_bencode_object(value)
                        .context("ut_metadata")
                        .map(Some)?;
                }
                (b"ut_pex", value) => {
                    ut_pex = u8::decode_bencode_object(value)
                        .context("ut_pex")
                        .map(Some)?;
                }
                _ => {}
            }
        }
        Ok(Self {
            ut_metadata,
            ut_pex,
        })
    }
}

impl ToBencode for Extension {
    const MAX_DEPTH: usize = 20;
    fn encode(
        &self,
        encoder: bendy::encoding::SingleItemEncoder,
    ) -> Result<(), bendy::encoding::Error> {
        encoder.emit_dict(|mut e| {
            e.emit_pair(b"m", &self.m)?;
            if let Some(metadata_size) = self.metadata_size {
                e.emit_pair(b"metadata_size", metadata_size)?;
            }
            if let Some(p) = self.p {
                e.emit_pair(b"p", p)?;
            }
            if let Some(reqq) = self.reqq {
                e.emit_pair(b"reqq", reqq)?;
            }
            if let Some(v) = &self.v {
                e.emit_pair(b"v", v)?;
            }
            Ok(())
        })
    }
}

impl FromBencode for Extension {
    fn decode_bencode_object(object: Object) -> Result<Self, bendy::decoding::Error>
    where
        Self: Sized,
    {
        let mut dict = object.try_into_dictionary()?;
        let mut p = None;
        let mut v = None;
        let mut reqq = None;
        let mut metadata_size = None;
        let mut m = M::default();

        while let Some(pair) = dict.next_pair()? {
            match pair {
                (b"m", value) => m = M::decode_bencode_object(value).context("m")?,
                (b"metadata_size", value) => {
                    metadata_size = u32::decode_bencode_object(value)
                        .context("metadata_size")
                        .map(Some)?;
                }
                (b"p", value) => {
                    p = u16::decode_bencode_object(value).context("p").map(Some)?;
                }
                (b"reqq", value) => {
                    reqq = u16::decode_bencode_object(value)
                        .context("reqq")
                        .map(Some)?;
                }
                (b"v", value) => {
                    v = String::decode_bencode_object(value)
                        .context("v")
                        .map(Some)?;
                }
                _ => {}
            }
        }
        Ok(Self {
            m,
            p,
            v,
            reqq,
            metadata_size,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // should transform a byte array into an Extension
    #[test]
    fn from_bytes_to_extension() {
        let bytes = [
            100, 49, 58, 101, 105, 49, 101, 49, 58, 109, 100, 49, 49, 58, 117, 116, 95, 109, 101,
            116, 97, 100, 97, 116, 97, 105, 51, 101, 54, 58, 117, 116, 95, 112, 101, 120, 105, 49,
            101, 101, 49, 51, 58, 109, 101, 116, 97, 100, 97, 116, 97, 95, 115, 105, 122, 101, 105,
            53, 50, 48, 53, 101, 49, 58, 112, 105, 53, 49, 52, 49, 51, 101, 52, 58, 114, 101, 113,
            113, 105, 53, 49, 50, 101, 49, 49, 58, 117, 112, 108, 111, 97, 100, 95, 111, 110, 108,
            121, 105, 49, 101, 49, 58, 118, 49, 55, 58, 84, 114, 97, 110, 115, 109, 105, 115, 115,
            105, 111, 110, 32, 50, 46, 57, 52, 101,
        ];

        let s = String::from_utf8_lossy(&bytes);
        println!("{s:?}");
        // d1:ei1e1:md11:ut_metadatai3e6:ut_pexi1ee13:metadata_sizei5205e1:pi51413e4:reqqi512e11:upload_onlyi1e1:v17:Transmission 2.94e

        let ext = Extension::from_bencode(&bytes).unwrap();

        assert_eq!(
            ext,
            Extension {
                m: M {
                    ut_metadata: Some(3),
                    ut_pex: Some(1),
                },
                p: Some(51413),
                v: Some("Transmission 2.94".to_owned()),
                reqq: Some(512),
                metadata_size: Some(5205),
            }
        );
    }

    // should get a byte array, and encode to an Extension
    // should ignore all data structures that we do not care about
    #[test]
    fn from_extension_to_bytes() {
        let bytes = [
            100, 49, 58, 101, 105, 49, 101, 49, 58, 109, 100, 49, 49, 58, 117, 116, 95, 109, 101,
            116, 97, 100, 97, 116, 97, 105, 51, 101, 54, 58, 117, 116, 95, 112, 101, 120, 105, 49,
            101, 101, 49, 51, 58, 109, 101, 116, 97, 100, 97, 116, 97, 95, 115, 105, 122, 101, 105,
            53, 50, 48, 53, 101, 49, 58, 112, 105, 53, 49, 52, 49, 51, 101, 52, 58, 114, 101, 113,
            113, 105, 53, 49, 50, 101, 49, 49, 58, 117, 112, 108, 111, 97, 100, 95, 111, 110, 108,
            121, 105, 49, 101, 49, 58, 118, 49, 55, 58, 84, 114, 97, 110, 115, 109, 105, 115, 115,
            105, 111, 110, 32, 50, 46, 57, 52, 101,
        ];

        let ext = Extension::from_bencode(&bytes).unwrap();
        let extension_bytes = ext.to_bencode().unwrap();

        let extension = Extension::from_bencode(&extension_bytes).unwrap();

        assert_eq!(
            extension,
            Extension {
                m: M {
                    ut_metadata: Some(3),
                    ut_pex: Some(1),
                },
                p: Some(51413),
                v: Some("Transmission 2.94".to_owned()),
                reqq: Some(512),
                metadata_size: Some(5205),
            }
        );
    }
}
