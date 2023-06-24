use bendy::{
    decoding::{self, FromBencode, Object, ResultExt},
    encoding::{self, AsString, Error, SingleItemEncoder, ToBencode},
};

#[derive(Debug)]
pub struct MetaInfo {
    pub announce: String,
    pub announce_list: Option<Vec<Vec<String>>>,
    pub info: Info,
    pub comment: Option<String>,
    pub creation_date: Option<u64>,
    pub http_seeds: Option<Vec<String>>,
}

/// File related information (Single-file format)
#[derive(Debug)]
pub struct Info {
    pub piece_length: u64,
    pub pieces: Vec<u8>,
    pub name: String,
    pub file_length: u64,
}

impl ToBencode for MetaInfo {
    const MAX_DEPTH: usize = 20;

    fn encode(&self, encoder: SingleItemEncoder) -> Result<(), encoding::Error> {
        encoder.emit_dict(|mut e| {
            e.emit_pair(b"announce", &self.announce)?;

            if let Some(announce_list) = &self.announce_list {
                e.emit_pair(b"announce-list", announce_list)?;
            }

            if let Some(comment) = &self.comment {
                e.emit_pair(b"comment", comment)?;
            }

            if let Some(creation_date) = &self.creation_date {
                e.emit_pair(b"creation date", creation_date)?;
            }

            if let Some(seeds) = &self.http_seeds {
                // List is a simple iterable wrapper that allows to encode
                // any list like container as bencode list object.
                e.emit_pair(b"httpseeds", seeds)?;
            }

            e.emit_pair(b"info", &self.info)
        })?;

        Ok(())
    }
}

impl ToBencode for Info {
    const MAX_DEPTH: usize = 20;

    fn encode(&self, encoder: SingleItemEncoder) -> Result<(), Error> {
        encoder.emit_dict(|mut e| {
            e.emit_pair(b"length", &self.file_length)?;
            e.emit_pair(b"name", &self.name)?;
            e.emit_pair(b"piece length", &self.piece_length)?;
            e.emit_pair(b"pieces", AsString(&self.pieces))
        })?;
        Ok(())
    }
}

impl FromBencode for MetaInfo {
    fn decode_bencode_object(object: Object) -> Result<Self, decoding::Error>
    where
        Self: Sized,
    {
        let mut announce = None;
        let mut announce_list = None;
        let mut comment = None;
        let mut creation_date = None;
        let mut http_seeds = None;
        let mut info = None;

        let mut dict_dec = object.try_into_dictionary()?;
        while let Some(pair) = dict_dec.next_pair()? {
            match pair {
                (b"announce", value) => {
                    announce = String::decode_bencode_object(value)
                        .context("announce")
                        .map(Some)?;
                }
                (b"announce-list", value) => {
                    announce_list = Vec::decode_bencode_object(value)
                        .context("announce_list")
                        .map(Some)?;
                }
                (b"comment", value) => {
                    comment = String::decode_bencode_object(value)
                        .context("comment")
                        .map(Some)?;
                }
                (b"creation date", value) => {
                    creation_date = u64::decode_bencode_object(value)
                        .context("creation_date")
                        .map(Some)?;
                }
                (b"httpseeds", value) => {
                    http_seeds = Vec::decode_bencode_object(value)
                        .context("http_seeds")
                        .map(Some)?;
                }
                (b"info", value) => {
                    info = Info::decode_bencode_object(value)
                        .context("info")
                        .map(Some)?;
                }
                _ => {}
            }
        }

        let announce = announce.ok_or_else(|| decoding::Error::missing_field("announce"))?;
        let info = info.ok_or_else(|| decoding::Error::missing_field("info"))?;

        Ok(MetaInfo {
            announce,
            announce_list,
            info,
            comment,
            creation_date,
            http_seeds,
        })
    }
}

impl FromBencode for Info {
    fn decode_bencode_object(object: Object) -> Result<Self, decoding::Error>
    where
        Self: Sized,
    {
        let mut file_length = None;
        let mut name = None;
        let mut piece_length = None;
        let mut pieces = None;

        let mut dict_dec = object.try_into_dictionary()?;
        while let Some(pair) = dict_dec.next_pair()? {
            match pair {
                (b"length", value) => {
                    file_length = u64::decode_bencode_object(value)
                        .context("file.length")
                        .map(Some)?;
                }
                (b"name", value) => {
                    name = String::decode_bencode_object(value)
                        .context("name")
                        .map(Some)?;
                }
                (b"piece length", value) => {
                    piece_length = u64::decode_bencode_object(value)
                        .context("piece length")
                        .map(Some)?;
                }
                (b"pieces", value) => {
                    pieces = AsString::decode_bencode_object(value)
                        .context("pieces")
                        .map(|bytes| Some(bytes.0))?;
                }
                _ => {}
            }
        }

        let file_length =
            file_length.ok_or_else(|| decoding::Error::missing_field("file_length"))?;
        let name = name.ok_or_else(|| decoding::Error::missing_field("name"))?;
        let piece_length =
            piece_length.ok_or_else(|| decoding::Error::missing_field("piece_length"))?;
        let pieces = pieces.ok_or_else(|| decoding::Error::missing_field("pieces"))?;

        // Check that we discovered all necessary fields
        Ok(Info {
            file_length,
            name,
            piece_length,
            pieces,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_decode_single_file_torrent() -> Result<(), decoding::Error> {
        let torrent = include_bytes!("../debian.torrent");
        let torrent = MetaInfo::from_bencode(torrent)?;
        println!("info piece[0] {:?}", torrent.info.pieces[0]);
        println!("info piece[1] {:?}", torrent.info.pieces[1]);
        println!("info piece[2] {:?}", torrent.info.pieces[2]);
        println!("info pieces len {:?}", torrent.info.pieces.len());
        println!("info name {:?}", torrent.info.name);
        println!("info piece len {:?}", torrent.info.piece_length);
        println!("info file len {:?}", torrent.info.file_length);

        Ok(())
    }

    #[test]
    fn should_decode_multi_file_torrent() -> Result<(), decoding::Error> {
        let torrent = include_bytes!("../book.torrent");
        let torrent = MetaInfo::from_bencode(torrent)?;
        println!("info piece[0] {:?}", torrent.info.pieces[0]);
        println!("info piece[1] {:?}", torrent.info.pieces[1]);
        println!("info piece[2] {:?}", torrent.info.pieces[2]);
        println!("info pieces len {:?}", torrent.info.pieces.len());
        println!("info name {:?}", torrent.info.name);
        println!("info piece len {:?}", torrent.info.piece_length);
        println!("info file len {:?}", torrent.info.file_length);

        Ok(())
    }

    #[test]
    fn should_encode_single_file_torrent() -> Result<(), encoding::Error> {
        let torrent = MetaInfo {
        announce: "http://bttracker.debian.org:6969/announce".to_owned(),
        announce_list: None,
        comment: Some("\"Debian CD from cdimage.debian.org\"".to_owned()),
        creation_date: Some(1_520_682_848),
        http_seeds: Some(vec![
            "https://cdimage.debian.org/cdimage/release/9.4.0//srv/cdbuilder.debian.org/dst/deb-cd/weekly-builds/amd64/iso-cd/debian-9.4.0-amd64-netinst.iso".to_owned(),
            "https://cdimage.debian.org/cdimage/archive/9.4.0//srv/cdbuilder.debian.org/dst/deb-cd/weekly-builds/amd64/iso-cd/debian-9.4.0-amd64-netinst.iso".to_owned(),
        ]),
        info: Info {
            piece_length: 262_144,
            pieces: include_bytes!("../pieces.iso").to_vec(),
            name: "debian-9.4.0-amd64-netinst.iso".to_owned(),
            file_length: 305_135_616,
        },
    };

        let data = torrent.to_bencode()?;

        Ok(())
    }
}
