use std::collections::VecDeque;

use bendy::{
    decoding::{self, FromBencode, Object, ResultExt},
    encoding::{self, AsString, Error, SingleItemEncoder, ToBencode},
};

use crate::{
    error,
    tcp_wire::lib::{BlockInfo, BLOCK_LEN},
};

#[derive(Debug, PartialEq, Clone, Default)]
pub struct MetaInfo {
    pub announce: String,
    pub announce_list: Option<Vec<Vec<String>>>,
    pub info: Info,
    pub comment: Option<String>,
    pub creation_date: Option<u32>,
    pub http_seeds: Option<Vec<String>>,
}

/// File related information (Single-file format)
/// https://fileformats.fandom.com/wiki/Torrent_file
// in a multi file format, `name` is name of the directory
// `file_length` is specific to Single File format
// in a multi file format, `file_length` is replaced to `files`
#[derive(Debug, PartialEq, Clone, Default)]
pub struct Info {
    /// piece length - number of bytes in a piece
    pub piece_length: u32,
    /// A (byte) string consisting of the concatenation of all 20-byte SHA1 hash values, one per piece.
    pub pieces: Vec<u8>,
    /// name of the file
    pub name: String,
    /// length - bytes of the entire file
    pub file_length: Option<u32>,
    pub files: Option<Vec<File>>,
}

impl Into<VecDeque<BlockInfo>> for Info {
    fn into(self) -> VecDeque<BlockInfo> {
        let mut v = VecDeque::new();

        // get block_infos for a Multi File Info
        if let Some(files) = &self.files {
            for file in files {
                let mut block_infos = file.get_block_infos(self.piece_length);
                v.append(&mut block_infos);
            }
        }

        // get block_infos for a Single File Info
        if let Some(file_length) = self.file_length {
            let file = File {
                length: file_length,
                path: vec![self.name.clone()],
            };
            let mut block_infos = file.get_block_infos(self.piece_length);
            v.append(&mut block_infos);
        }

        v
    }
}

impl Info {
    /// Calculate how many blocks there are in the entire torrent.
    pub fn blocks_len(&self) -> u32 {
        let blocks_in_piece = self.blocks_per_piece();
        let total_blocks = blocks_in_piece * (self.pieces.len() as u32 / 20);
        total_blocks
    }
    /// Calculate how many blocks there are per piece
    pub fn blocks_per_piece(&self) -> u32 {
        self.piece_length / BLOCK_LEN
    }
    /// Get all block_infos of a torrent
    pub async fn get_block_infos(&self) -> Result<VecDeque<BlockInfo>, error::Error> {
        // multi file torrent
        if let Some(files) = &self.files {
            let file = files
                .into_iter()
                .enumerate()
                .find(|(i, f)| self.piece_length < f.length * (*i as u32 + 1))
                .map(|a| a.1);

            if let Some(file) = file {
                let infos = file.get_block_infos(self.piece_length);

                return Ok(infos);
            }

            return Err(error::Error::FileOpenError);
        }

        // single file torrent
        if let Some(length) = self.file_length {
            let file = File {
                length,
                path: vec![self.name.to_owned()],
            };
            let infos = file.get_block_infos(self.piece_length);
            return Ok(infos);
        }
        return Err(error::Error::FileOpenError);
    }
}

#[derive(Debug, PartialEq, Clone, Default)]
pub struct File {
    pub length: u32,
    pub path: Vec<String>,
}

impl File {
    pub fn get_piece_len(&self, piece: u32, piece_length: u32) -> u32 {
        // let last_piece_len = self.length % piece_length;
        // let last_piece_index = self.length / piece_length;
        let piece_end = (piece + 1) * piece_length;

        if piece_end > self.length {
            // piece does not fit, return the remainder
            piece_end - self.length
        } else {
            // piece fits, return default piece len
            piece_length
        }
    }
    pub fn get_block_infos(&self, piece_length: u32) -> VecDeque<BlockInfo> {
        let mut infos: VecDeque<BlockInfo> = VecDeque::new();
        let pieces = self.length as f32 / piece_length as f32;
        let pieces = pieces.ceil();

        // bytes of entire file
        // todo: get this on argument,
        // let file_length = pieces as u32 * piece_length;
        let pieces = pieces as u32;

        // println!("{:?} self.length", self.length);
        // println!("{piece_length} piece_len");
        // println!("{pieces} pieces");
        // println!("{torrent_len:?} torrent_len\n");

        let mut index = 0 as u32;

        let last_block_len = if self.length % BLOCK_LEN == 0 {
            BLOCK_LEN
        } else {
            self.length % BLOCK_LEN
        };

        for piece in 0..pieces {
            let piece_len = self.get_piece_len(piece, piece_length);

            let is_last_piece = pieces == piece + 1;
            let blocks_per_piece = piece_len as f32 / BLOCK_LEN as f32;
            let blocks_per_piece = blocks_per_piece.ceil() as u32;

            for block in 0..blocks_per_piece {
                let begin = if blocks_per_piece == 1 {
                    0
                } else {
                    infos.len() as u32 * BLOCK_LEN as u32
                };
                let is_last_block = blocks_per_piece == block + 1;

                let len = BLOCK_LEN % (begin + BLOCK_LEN);

                // if the bytes to be written are larger than the
                // size of the current file, write only the bytes
                // that fits the max amount, which is the file bytes
                let len = if len > self.length {
                    len - self.length
                } else {
                    BLOCK_LEN
                };

                let len = if is_last_piece && is_last_block {
                    last_block_len
                } else {
                    len
                };

                let bi = BlockInfo { index, begin, len };
                infos.push_back(bi);

                if is_last_block {
                    index += 1;
                }
            }
        }
        infos
    }
}

impl ToBencode for File {
    const MAX_DEPTH: usize = 5;

    fn encode(&self, encoder: SingleItemEncoder) -> Result<(), Error> {
        encoder.emit_dict(|mut e| {
            e.emit_pair(b"length", &self.length)?;
            e.emit_pair(b"path", &self.path)
        })?;
        Ok(())
    }
}

impl FromBencode for File {
    fn decode_bencode_object(object: Object) -> Result<Self, decoding::Error>
    where
        Self: Sized,
    {
        let mut dict_dec = object.try_into_dictionary()?;
        let mut length = 0;
        let mut path: Vec<String> = vec![];

        while let Some(pair) = dict_dec.next_pair()? {
            match pair {
                (b"length", value) => {
                    length = u32::decode_bencode_object(value).context("length")?;
                }
                (b"path", value) => {
                    path = Vec::<String>::decode_bencode_object(value).context("path")?;
                }
                _ => {}
            }
        }

        Ok(Self { length, path })
    }
}

impl ToBencode for MetaInfo {
    const MAX_DEPTH: usize = 5;

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
    const MAX_DEPTH: usize = 5;

    fn encode(&self, encoder: SingleItemEncoder) -> Result<(), Error> {
        encoder.emit_dict(|mut e| {
            if let Some(file_length) = &self.file_length {
                e.emit_pair(b"length", file_length)?;
            }
            if let Some(files) = &self.files {
                e.emit_pair(b"files", files)?;
            }
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
                    creation_date = u32::decode_bencode_object(value)
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
        let mut files = None;
        let mut file_length = None;
        let mut name = None;
        let mut piece_length = None;
        let mut pieces = None;

        let mut dict_dec = object.try_into_dictionary()?;
        while let Some(pair) = dict_dec.next_pair()? {
            match pair {
                (b"files", value) => {
                    files = Vec::<File>::decode_bencode_object(value)
                        .context("files")
                        .map(Some)?;
                }
                (b"length", value) => {
                    file_length = u32::decode_bencode_object(value)
                        .context("file.length")
                        .map(Some)?;
                }
                (b"name", value) => {
                    name = String::decode_bencode_object(value)
                        .context("name")
                        .map(Some)?;
                }
                (b"piece length", value) => {
                    piece_length = u32::decode_bencode_object(value)
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

        let name = name.ok_or_else(|| decoding::Error::missing_field("name"))?;
        let piece_length =
            piece_length.ok_or_else(|| decoding::Error::missing_field("piece_length"))?;
        let pieces = pieces.ok_or_else(|| decoding::Error::missing_field("pieces"))?;

        // Check that we discovered all necessary fields
        Ok(Info {
            files,
            file_length,
            name,
            piece_length,
            pieces,
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::error::Error;

    use super::*;

    #[tokio::test]
    async fn should_get_block_infos() -> Result<(), encoding::Error> {
        let torrent_book_bytes = include_bytes!("../book.torrent");

        let torrent = MetaInfo::from_bencode(torrent_book_bytes).unwrap();
        let info = torrent.info;

        let blocks_in_piece = info.piece_length / BLOCK_LEN;
        let total_blocks = blocks_in_piece * (info.pieces.len() as u32 / 20);

        println!("--- book torrent ----");
        println!("\npiece_length {:?}", info.piece_length);
        println!("block_len {:?}", BLOCK_LEN);
        println!("bip {blocks_in_piece:?}");
        println!("tb {total_blocks:?}");

        let infos = info.get_block_infos().await.unwrap();

        assert_eq!(250, infos.len());
        println!("\ninfos 0 {:#?}", infos[0]);
        println!("\ninfos 1 {:#?}", infos[1]);
        println!("\ninfos 2 {:#?}", infos[2]);
        println!("\ninfos last {:#?}", infos.back());

        assert_eq!(
            infos[0],
            BlockInfo {
                index: 0,
                begin: 0,
                len: 16384,
            }
        );
        assert_eq!(
            infos[1],
            BlockInfo {
                index: 1,
                begin: 0,
                len: 16384,
            }
        );
        assert_eq!(
            infos[2],
            BlockInfo {
                index: 2,
                begin: 0,
                len: 16384,
            }
        );
        assert_eq!(
            *infos.back().unwrap(),
            BlockInfo {
                index: 249,
                begin: 0,
                len: 12718,
            }
        );
        let last = infos.back().clone().unwrap();
        assert_eq!(
            *last,
            BlockInfo {
                index: 249,
                begin: 0,
                len: 12718,
            }
        );

        let torrent_len = info.files.unwrap().iter().fold(0, |acc, f| acc + f.length);
        let sum = ((total_blocks - 1) * BLOCK_LEN) + last.len;

        assert_eq!(sum, torrent_len);

        let torrent_book_bytes = include_bytes!("../debian.torrent");

        let torrent = MetaInfo::from_bencode(torrent_book_bytes).unwrap();
        let info = torrent.info;

        let total_blocks = info.blocks_len();

        println!("--- debian torrent ----");
        println!("piece_length {:?}", info.piece_length);
        println!("block_len {:?}", BLOCK_LEN);
        println!("blocks in piece {blocks_in_piece:?}");
        println!("total blocks in file {total_blocks:?}");

        let infos = info.get_block_infos().await.unwrap();

        assert_eq!(18624, infos.len());
        println!("\ninfos 0 {:#?}", infos[0]);
        println!("\ninfos 1 {:#?}", infos[1]);
        println!("\ninfos 2 {:#?}", infos[2]);
        println!("\ninfos last {:#?}", infos.back());

        let last = infos.back().unwrap().clone();

        assert_eq!(
            infos[0],
            BlockInfo {
                index: 0,
                begin: 0,
                len: 16384,
            }
        );
        assert_eq!(
            infos[1],
            BlockInfo {
                index: 0,
                begin: 16384,
                len: 16384,
            }
        );
        assert_eq!(
            infos[2],
            BlockInfo {
                index: 0,
                begin: 32768,
                len: 16384,
            }
        );
        assert_eq!(
            *infos.back().unwrap(),
            BlockInfo {
                index: 1163,
                begin: 305119232,
                len: 16384,
            }
        );

        assert_eq!(info.file_length.unwrap(), last.begin + last.len);

        Ok(())
    }

    #[test]
    fn should_encode_multi_file_torrent() -> Result<(), encoding::Error> {
        let torrent_book_bytes = include_bytes!("../book.torrent");

        let torrent = MetaInfo::from_bencode(torrent_book_bytes).unwrap();
        let torrent_bytes = torrent.to_bencode().unwrap();
        let torrent = MetaInfo::from_bencode(&torrent_bytes).unwrap();

        assert_eq!(torrent_bytes, torrent.to_bencode()?);

        Ok(())
    }

    #[test]
    fn should_decode_multi_file_torrent() -> Result<(), decoding::Error> {
        let torrent = include_bytes!("../book.torrent");
        let torrent = MetaInfo::from_bencode(torrent)?;

        assert_eq!(torrent, {
            MetaInfo {
                creation_date: Some(1_662_883_480),
                http_seeds: None,
                comment: Some("dynamic metainfo from client".to_owned()),
                announce: "udp://tracker.leechers-paradise.org:6969/announce".to_owned(),
                announce_list: Some(vec![
                    vec!["udp://tracker.leechers-paradise.org:6969/announce".to_owned()],
                    vec!["udp://tracker.internetwarriors.net:1337/announce".to_owned()],
                    vec!["udp://tracker.opentrackr.org:1337/announce".to_owned()],
                    vec!["udp://tracker.coppersurfer.tk:6969/announce".to_owned()],
                    vec!["udp://tracker.pirateparty.gr:6969/announce".to_owned()],
                    vec!["udp://9.rarbg.to:2730/announce".to_owned()],
                    vec!["udp://9.rarbg.to:2710/announce".to_owned()],
                    vec!["udp://bt.xxx-tracker.com:2710/announce".to_owned()],
                    vec!["udp://tracker.cyberia.is:6969/announce".to_owned()],
                    vec!["udp://retracker.lanta-net.ru:2710/announce".to_owned()],
                    vec!["udp://9.rarbg.to:2770/announce".to_owned()],
                    vec!["udp://9.rarbg.me:2730/announce".to_owned()],
                    vec!["udp://eddie4.nl:6969/announce".to_owned()],
                    vec!["udp://tracker.mg64.net:6969/announce".to_owned()],
                    vec!["udp://open.demonii.si:1337/announce".to_owned()],
                    vec!["udp://tracker.zer0day.to:1337/announce".to_owned()],
                    vec!["udp://tracker.tiny-vps.com:6969/announce".to_owned()],
                    vec!["udp://ipv6.tracker.harry.lu:80/announce".to_owned()],
                    vec!["udp://9.rarbg.me:2740/announce".to_owned()],
                    vec!["udp://9.rarbg.me:2770/announce".to_owned()],
                    vec!["udp://denis.stalker.upeer.me:6969/announce".to_owned()],
                    vec!["udp://tracker.port443.xyz:6969/announce".to_owned()],
                    vec!["udp://tracker.moeking.me:6969/announce".to_owned()],
                    vec!["udp://exodus.desync.com:6969/announce".to_owned()],
                    vec!["udp://9.rarbg.to:2740/announce".to_owned()],
                    vec!["udp://9.rarbg.to:2720/announce".to_owned()],
                    vec!["udp://tracker.justseed.it:1337/announce".to_owned()],
                    vec!["udp://tracker.torrent.eu.org:451/announce".to_owned()],
                    vec!["udp://ipv4.tracker.harry.lu:80/announce".to_owned()],
                    vec!["udp://tracker.open-internet.nl:6969/announce".to_owned()],
                    vec!["udp://torrentclub.tech:6969/announce".to_owned()],
                    vec!["udp://open.stealth.si:80/announce".to_owned()],
                    vec!["http://tracker.tfile.co:80/announce".to_owned()],
                ]),
                info: Info {
                    piece_length: 163_84,
                    pieces: torrent.info.pieces.clone(),
                    name: "Kerkour S. Black Hat Rust...Rust programming language 2022".to_owned(),
                    files: Some(vec![File {
                        length: 4092334,
                        path: vec![
                            "Kerkour S. Black Hat Rust...Rust programming language 2022.pdf"
                                .to_owned(),
                        ],
                    }]),
                    file_length: None,
                },
            }
        });

        Ok(())
    }

    #[test]
    fn should_decode_single_file_torrent() -> Result<(), decoding::Error> {
        let torrent = include_bytes!("../debian.torrent");
        let torrent = MetaInfo::from_bencode(torrent)?;

        assert_eq!(torrent, {
            MetaInfo {
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
                    files: None,
                    file_length: Some(305_135_616),
                },
            }
        });

        // each hash value of the piece has 20 bytes, so divide by 20
        // to get the len of 1 piece
        println!(
            "info proof of file bytes {:?}",
            torrent.info.piece_length as usize * (torrent.info.pieces.len() / 20)
        );

        Ok(())
    }

    #[test]
    fn should_encode_single_file_torrent() -> Result<(), encoding::Error> {
        let torrent_disk = include_bytes!("../debian.torrent");

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
                files: None,
                file_length: Some(305_135_616),
            },
        };

        let data = torrent.to_bencode()?;

        assert_eq!(torrent_disk.to_vec(), data);

        Ok(())
    }

    #[test]
    fn should_encode_file() -> Result<(), encoding::Error> {
        let file = File {
            path: ["a".to_owned(), "b".to_owned(), "c.txt".to_owned()].into(),
            length: 222,
        };

        let data = file.to_bencode()?;

        assert_eq!(
            String::from_utf8(data).unwrap(),
            "d6:lengthi222e4:pathl1:a1:b5:c.txtee".to_owned()
        );

        Ok(())
    }

    #[test]
    fn should_decode_file() -> Result<(), decoding::Error> {
        let data = b"d6:lengthi222e4:pathl1:a1:b5:c.txtee";

        let file = File::from_bencode(data)?;

        assert_eq!(
            file,
            File {
                path: ["a".to_owned(), "b".to_owned(), "c.txt".to_owned()].into(),
                length: 222,
            }
        );

        Ok(())
    }
}
