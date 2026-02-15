//! Metainfo is a .torrent file with information abouthe Torrent.
//! From the magnet link, we get the Metainfo from other peers.

use crate::{
    error,
    extensions::core::{BLOCK_LEN, BlockInfo},
    torrent::InfoHash,
};
use bendy::{
    decoding::{self, Decoder, FromBencode, Object, ResultExt},
    encoding::{self, AsString, Error, SingleItemEncoder, ToBencode},
};
use sha1::{Digest, Sha1};
use std::collections::{BTreeMap, HashMap};

/// Metainfo is a .torrent file with information about the Torrent.
/// From the magnet link, we get the Metainfo from other peers.
#[derive(Debug, PartialEq, Clone, Default)]
pub struct MetaInfo {
    pub announce: String,
    pub announce_list: Option<Vec<Vec<String>>>,
    pub comment: Option<String>,
    // pub created_by: Option<String>,
    pub creation_date: Option<u32>,
    pub info: Info,
    pub http_seeds: Option<Vec<String>>,
}

impl MetaInfo {
    pub fn organize_trackers(&self) -> HashMap<&str, Vec<String>> {
        let mut hashmap = HashMap::from([
            ("udp", vec![]),
            ("http", vec![]),
            ("https", vec![]),
        ]);

        let mut list = vec![self.announce.clone()];

        if let Some(l) = self.announce_list.clone() {
            list.pop();
            list.extend(l.into_iter().flatten());
        }

        for x in list {
            let x = x.clone();
            let mut uri = urlencoding::decode(&x).unwrap().to_string();
            let (protocol, _) = x.split_once("%3A").unwrap();

            if protocol == "udp" {
                uri = uri.replace("udp://", "");
                // remove any /announce
                if let Some(i) = uri.find("/announce") {
                    uri = uri[..i].to_string();
                };
            }

            let trackers = hashmap.get_mut(protocol).unwrap();

            trackers.push(uri.to_string());
        }

        hashmap
    }
}

/// File related information (Single-file format)
/// <https://fileformats.fandom.com/wiki/Torrent_file>
/// in a multi file format, `name` is name of the directory
/// `file_length` is specific to Single File format
/// in a multi file format, `file_length` is replaced to `files`
#[derive(Debug, PartialEq, Clone, Default)]
pub struct Info {
    /// Some seeders want to cross-seed the same torrent for multiple trackers,
    /// but if the info hash is the same, most clients will block it.
    /// With this entry, the info_hash changes and cross-seeding is now
    /// possible.
    pub cross_seed_entry: Option<[u8; 32]>,

    /// If the torrent has only 1 file, this value is some, and files is none
    pub file_length: Option<usize>,

    /// If the torrent has many files, this is some, and file_length is none.
    pub files: Option<Vec<File>>,

    /// name of the file
    pub name: String,

    /// length in bytes of each piece, the last piece may have a smaller length
    pub piece_length: usize,

    /// A (byte) string consisting of the concatenation of all 20-byte SHA1
    /// hash values, one per piece.
    pub pieces: Vec<u8>,

    /// The torrent's source, usually the tracker's website.
    pub source: Option<String>,

    // the following is internal computed data for better ergonomics, and
    // not included in the real Info.
    pub metadata_size: usize,
    pub info_hash: InfoHash,
}

impl Info {
    pub fn to_meta_info(self, announce_list: &[String]) -> MetaInfo {
        let first_item = announce_list.first().cloned().unwrap_or_default();

        let announce = announce_list
            .iter()
            .find(|v| v.starts_with("udp"))
            .cloned()
            .unwrap_or(first_item);

        let mut list = vec![
            vec![], // udp
            vec![], // http
            vec![], // https
            vec![], // wss
        ];

        for l in announce_list {
            if l.starts_with("udp") {
                list[0].push(l.to_owned());
            } else if l.starts_with("http") {
                list[1].push(l.to_owned());
            } else if l.starts_with("https") {
                list[2].push(l.to_owned());
            } else if l.starts_with("wss") {
                list[3].push(l.to_owned());
            }
        }

        MetaInfo {
            announce_list: Some(list),
            announce,
            info: self,
            ..Default::default()
        }
    }

    pub fn name(mut self, name: String) -> Self {
        self.name = name;
        self
    }

    /// Calculate how many pieces there are.
    #[inline]
    pub fn pieces(&self) -> usize {
        (self.pieces.len()).div_ceil(20)
    }

    /// Calculate how many blocks there are in the entire torrent.
    #[inline]
    pub fn blocks_count(&self) -> usize {
        self.get_torrent_size().div_ceil(BLOCK_LEN)
    }

    /// Calculate how many blocks there are per piece
    #[inline]
    pub fn blocks_per_piece(&self) -> usize {
        self.piece_length.div_ceil(BLOCK_LEN)
    }

    pub(crate) fn info_hash(buf: &[u8]) -> InfoHash {
        let mut hasher = Sha1::new();
        hasher.update(buf);
        InfoHash(hasher.finalize().into())
    }

    pub fn get_block_infos_of_piece_self(
        &self,
        piece_index: usize,
    ) -> Vec<BlockInfo> {
        let total_size = self.get_torrent_size();
        let piece_length = self.piece_length;
        let piece_start = piece_index * piece_length;
        let piece_end = (piece_start + piece_length).min(total_size);
        let piece_size = piece_end - piece_start;

        // calculate all blocks for this piece in one go
        let num_blocks = piece_size.div_ceil(BLOCK_LEN);
        let mut blocks = Vec::with_capacity(num_blocks);

        for block_index in 0..num_blocks {
            let begin = block_index * BLOCK_LEN;
            let len = if block_index == num_blocks - 1 {
                piece_size - begin
            } else {
                BLOCK_LEN
            };

            blocks.push(BlockInfo { index: piece_index, begin, len });
        }

        blocks
    }

    /// Only get block infos of a piece.
    pub fn get_block_infos_of_piece(
        total_size: usize,
        piece_length: usize,
        piece_index: usize,
    ) -> Vec<BlockInfo> {
        let piece_start = piece_index * piece_length;
        let piece_end = (piece_start + piece_length).min(total_size);
        let piece_size = piece_end - piece_start;

        // calculate all blocks for this piece in one go
        let num_blocks = piece_size.div_ceil(BLOCK_LEN);
        let mut blocks = Vec::with_capacity(num_blocks);

        for block_index in 0..num_blocks {
            let begin = block_index * BLOCK_LEN;
            let len = if block_index == num_blocks - 1 {
                piece_size - begin
            } else {
                BLOCK_LEN
            };

            blocks.push(BlockInfo { index: piece_index, begin, len });
        }

        blocks
    }

    /// Get all block_infos of a torrent
    /// Returns an Err if the Info is malformed, if it does not have `files` or
    /// `file_length`.
    pub fn get_block_infos(
        &self,
    ) -> Result<BTreeMap<usize, Vec<BlockInfo>>, error::Error> {
        let total_size = self.get_torrent_size();
        let piece_length = self.piece_length;
        let num_pieces = self.pieces();
        let mut block_infos: BTreeMap<usize, Vec<BlockInfo>> = BTreeMap::new();

        for piece_index in 0..num_pieces {
            let blocks = Self::get_block_infos_of_piece(
                total_size,
                piece_length,
                piece_index,
            );

            block_infos.insert(piece_index, blocks);
        }

        Ok(block_infos)
    }

    /// Get the size in bytes of the files of the torrent.
    pub fn get_torrent_size(&self) -> usize {
        match &self.files {
            Some(files) => files.iter().map(|f| f.length).sum(),
            None => self.file_length.unwrap_or(0),
        }
    }

    /// Get the size (in bytes) of a piece.
    pub fn piece_size(&self, piece_index: usize) -> usize {
        let total_size = self.get_torrent_size();
        if piece_index == self.pieces() - 1 {
            let remainder = total_size % self.piece_length;
            if remainder == 0 { self.piece_length } else { remainder }
        } else {
            self.piece_length
        }
    }
}

/// Files in the [`Info`] are relative to the root folder name,
/// but do not contain them as the first item in the vector.
#[derive(Debug, PartialEq, Clone, Default, Hash, Eq)]
pub struct File {
    /// Length of the file in bytes.
    pub length: usize,
    /// Path of the file, excluding the parent name.
    pub path: Vec<String>,
}

impl File {
    /// Get the len of the given piece in the file, in bytes..
    pub fn get_piece_len(&self, piece: usize, piece_length: usize) -> usize {
        let b = (piece * piece_length) + piece_length;
        if b <= self.length { piece_length } else { self.length % piece_length }
    }
    /// Return the number of pieces in the file, rounded up.
    pub fn pieces(&self, piece_length: usize) -> usize {
        self.length.div_ceil(piece_length)
    }
}

impl ToBencode for File {
    const MAX_DEPTH: usize = 5;

    fn encode(&self, encoder: SingleItemEncoder) -> Result<(), Error> {
        encoder.emit_dict(|mut e| {
            e.emit_pair(b"length", self.length)?;
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
                    length = usize::decode_bencode_object(value)
                        .context("length")?;
                }
                (b"path", value) => {
                    path = Vec::<String>::decode_bencode_object(value)
                        .context("path")?;
                }
                _ => {}
            }
        }

        Ok(Self { length, path })
    }
}

impl ToBencode for MetaInfo {
    const MAX_DEPTH: usize = 5;

    fn encode(
        &self,
        encoder: SingleItemEncoder,
    ) -> Result<(), encoding::Error> {
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
            e.emit_pair(b"piece length", self.piece_length)?;
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

        let announce = announce
            .ok_or_else(|| decoding::Error::missing_field("announce"))?;
        let info =
            info.ok_or_else(|| decoding::Error::missing_field("info"))?;

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
        let mut cross_seed_entry = None;
        let mut files = None;
        let mut file_length = None;
        let mut name = None;
        let mut source = None;
        let mut piece_length = None;
        let mut pieces = None;

        let bytes = object.try_into_dictionary()?;
        let bytes = bytes.into_raw()?;
        let size = bytes.len();
        let info_hash = Info::info_hash(bytes);
        let mut decoder = Decoder::new(bytes);
        let mut dict = decoder.next_object()?.unwrap().try_into_dictionary()?;

        while let Some(pair) = dict.next_pair()? {
            match pair {
                (b"cross_seed_entry", value) => {
                    cross_seed_entry = AsString::decode_bencode_object(value)
                        .context("cross_seed_entry")
                        .map(|bytes| {
                            if bytes.0.len() < 32 {
                                return None;
                            }
                            let mut buff = [0u8; 32];
                            for (i, v) in bytes.0.into_iter().enumerate() {
                                buff[i] = v;
                            }
                            Some(buff)
                        })?;
                }
                (b"files", value) => {
                    files = Vec::<File>::decode_bencode_object(value)
                        .context("files")
                        .map(Some)?;
                }
                (b"length", value) => {
                    file_length = usize::decode_bencode_object(value)
                        .context("file.length")
                        .map(Some)?;
                }
                (b"name", value) => {
                    name = String::decode_bencode_object(value)
                        .context("name")
                        .map(Some)?;
                }
                (b"source", value) => {
                    source = String::decode_bencode_object(value)
                        .context("source")
                        .map(Some)?;
                }
                (b"piece length", value) => {
                    piece_length = usize::decode_bencode_object(value)
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

        let name =
            name.ok_or_else(|| decoding::Error::missing_field("name"))?;
        let piece_length = piece_length
            .ok_or_else(|| decoding::Error::missing_field("piece_length"))?;
        let pieces =
            pieces.ok_or_else(|| decoding::Error::missing_field("pieces"))?;

        // Check that we discovered all necessary fields
        Ok(Info {
            cross_seed_entry,
            files,
            file_length,
            name,
            piece_length,
            pieces,
            source,
            metadata_size: size,
            info_hash,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_info(total_size: usize, piece_length: usize) -> Info {
        let pieces_len =
            (total_size as f64 / piece_length as f64).ceil() as usize * 20;
        Info {
            file_length: Some(total_size),
            name: "test".to_string(),
            piece_length,
            pieces: vec![0; pieces_len],
            ..Default::default()
        }
    }

    /// piece_length: 15
    /// -------------------
    /// | f: 30           |
    /// ---------p---------
    /// | b: 15  | b: 15  |
    /// -------------------
    #[test]
    fn get_block_infos_smaller_than_block_info() {
        let info = create_test_info(30, 15);
        assert_eq!(
            *info.get_block_infos().unwrap().get(&0).unwrap(),
            Vec::from([BlockInfo { index: 0, begin: 0, len: 15 },]),
        );
        assert_eq!(
            *info.get_block_infos().unwrap().get(&1).unwrap(),
            Vec::from([BlockInfo { index: 1, begin: 0, len: 15 },]),
        );
    }

    /// piece_length: 16384
    /// -------------------------------------
    /// | f: 32868                          |
    /// -------------p-------------p---------
    /// | b: 16384   | b: 16384    | b: 100 |
    /// -------------------------------------
    #[test]
    fn get_block_infos_one_block_piece() {
        let info = create_test_info(32868, BLOCK_LEN);
        let blocks = info.get_block_infos().unwrap();
        assert_eq!(
            *blocks.get(&0).unwrap(),
            Vec::from([BlockInfo { index: 0, begin: 0, len: BLOCK_LEN },]),
        );
        assert_eq!(
            *blocks.get(&1).unwrap(),
            Vec::from([BlockInfo { index: 1, begin: 0, len: BLOCK_LEN },]),
        );
        assert_eq!(
            *blocks.get(&2).unwrap(),
            Vec::from([BlockInfo { index: 2, begin: 0, len: 100 }]),
        );
    }

    /// piece_length: 16384
    /// ----------------------------
    /// | f: 32768                 |
    /// -------------p--------------
    /// | b: 16384   | b: 16384    |
    /// ----------------------------
    #[test]
    fn get_block_infos_even() {
        let info = create_test_info(32768, BLOCK_LEN);
        let blocks = info.get_block_infos().unwrap();
        assert_eq!(
            *blocks.get(&0).unwrap(),
            vec![BlockInfo { index: 0, begin: 0, len: BLOCK_LEN },]
        );
        assert_eq!(
            *blocks.get(&1).unwrap(),
            vec![BlockInfo { index: 1, begin: 0, len: BLOCK_LEN },]
        );
    }

    /// piece_length: 32660
    /// ----------------------------
    /// | f: 32768                 |
    /// --------------------------p-
    /// | b: 16384   | b: 16384    |
    /// ----------------------------
    #[test]
    fn get_block_infos_odd() {
        let info = create_test_info(32768, 32668);
        let blocks = info.get_block_infos().unwrap();
        assert_eq!(
            *blocks.get(&0).unwrap(),
            vec![
                BlockInfo { index: 0, begin: 0, len: BLOCK_LEN },
                BlockInfo { index: 0, begin: BLOCK_LEN, len: 16284 },
            ]
        );
        assert_eq!(
            *blocks.get(&1).unwrap(),
            vec![BlockInfo { index: 1, begin: 0, len: 100 },]
        );
    }

    /// piece_length: 32668
    /// --------------------------------------
    /// | f: 10 | f: 32768                   |
    /// ----------------------------------p---
    /// | b: 10 | b: 16384   | b: 16274  |110|
    /// --------------------------------------
    #[test]
    fn get_block_infos_odd_pre() {
        let info = create_test_info(32778, 32668);
        let blocks = info.get_block_infos().unwrap();

        // check the pieces size
        assert_eq!(info.piece_size(0), 32668);
        assert_eq!(info.piece_size(1), 110);

        assert_eq!(
            *blocks.get(&0).unwrap(),
            vec![
                BlockInfo { index: 0, begin: 0, len: BLOCK_LEN },
                BlockInfo { index: 0, begin: BLOCK_LEN, len: 16284 },
            ]
        );

        assert_eq!(
            *blocks.get(&1).unwrap(),
            Vec::from([BlockInfo { index: 1, begin: 0, len: 110 },])
        );
    }

    /// piece_length: 32668
    /// ------------------------------
    /// | f: 32768            | f 10 |
    /// ------------------p-----------
    /// |b: 16384|b: 16284|100|  10  |
    /// ------------------------------
    #[test]
    fn get_block_infos_odd_post() {
        let info = create_test_info(32778, 32668);
        let blocks = info.get_block_infos().unwrap();

        // check the pieces size
        assert_eq!(info.piece_size(0), 32668);
        assert_eq!(info.piece_size(1), 110);

        assert_eq!(
            *blocks.get(&0).unwrap(),
            vec![
                BlockInfo { index: 0, begin: 0, len: BLOCK_LEN },
                BlockInfo { index: 0, begin: BLOCK_LEN, len: 16284 },
            ]
        );

        assert_eq!(
            *blocks.get(&1).unwrap(),
            vec![BlockInfo { index: 1, begin: 0, len: 110 },]
        );
    }

    #[test]
    fn get_block_infos_partial_last_block() {
        let info = create_test_info(BLOCK_LEN + 1, BLOCK_LEN);
        let blocks = info.get_block_infos().unwrap();

        assert_eq!(blocks.len(), 2);
        assert_eq!(
            blocks[&0],
            vec![BlockInfo { index: 0, begin: 0, len: BLOCK_LEN }]
        );
        assert_eq!(blocks[&1], vec![BlockInfo { index: 1, begin: 0, len: 1 }]);
    }

    #[test]
    fn get_block_infos_empty_torrent() {
        let info = create_test_info(0, BLOCK_LEN);
        let blocks = info.get_block_infos().unwrap();
        assert!(blocks.is_empty());
    }

    #[test]
    fn get_block_infos_single_block() {
        let info = create_test_info(10000, BLOCK_LEN);
        let blocks = info.get_block_infos().unwrap();

        assert_eq!(blocks.len(), 1);
        assert_eq!(
            blocks[&0],
            vec![BlockInfo { index: 0, begin: 0, len: 10000 }]
        );
    }

    #[test]
    fn test_piece_size() {
        let info = create_test_info(32768, BLOCK_LEN);
        assert_eq!(info.piece_size(0), BLOCK_LEN);
        assert_eq!(info.piece_size(1), BLOCK_LEN);

        let info = create_test_info(32769, BLOCK_LEN);
        assert_eq!(info.piece_size(0), BLOCK_LEN);
        assert_eq!(info.piece_size(1), BLOCK_LEN);
        assert_eq!(info.piece_size(2), 1); // last piece

        // Piece length not divisible by block size
        let info = create_test_info(33000, 17000);
        assert_eq!(info.piece_size(0), 17000);
        assert_eq!(info.piece_size(1), 16000);
    }

    #[test]
    fn test_blocks_per_piece() {
        let info = create_test_info(100_000, 16_384);
        assert_eq!(info.blocks_per_piece(), 1);

        let info = create_test_info(100_000, 32_768);
        assert_eq!(info.blocks_per_piece(), 2);

        let info = create_test_info(100_000, 10_000);
        assert_eq!(info.blocks_per_piece(), 1); // 10,000 / 16,384 = 0.61 → ceil
        // to 1
    }

    #[test]
    fn test_total_blocks() {
        // Exact block boundaries
        let info = create_test_info(16384, 16384);
        assert_eq!(info.blocks_count(), 1);

        // Partial block
        let info = create_test_info(16385, 16384);
        assert_eq!(info.blocks_count(), 2);

        // Multiple files
        let info = Info {
            files: Some(vec![
                File { length: 16384, path: vec![] },
                File { length: 1, path: vec![] },
            ]),
            ..create_test_info(0, 16384)
        };
        assert_eq!(info.blocks_count(), 2);

        // Empty torrent
        let info = create_test_info(0, 16384);
        assert_eq!(info.blocks_count(), 0);
    }

    /// Confirm that the [`MetaInfo`] [`FromBencode`] implementation works as
    /// expected for a multi-file torrent
    #[test]
    fn should_decode_multi_file_torrent() -> Result<(), decoding::Error> {
        let torrent = include_bytes!("../../../test-files/book.torrent");
        let torrent = MetaInfo::from_bencode(torrent)?;

        assert_eq!(torrent, {
            MetaInfo {
                creation_date: Some(1_662_883_480),
                http_seeds: None,
                comment: Some("dynamic metainfo from client".to_owned()),
                announce: "udp://tracker.leechers-paradise.org:6969/announce"
                    .to_owned(),
                announce_list: Some(vec![
                    vec![
                        "udp://tracker.leechers-paradise.org:6969/announce"
                            .to_owned(),
                    ],
                    vec![
                        "udp://tracker.internetwarriors.net:1337/announce"
                            .to_owned(),
                    ],
                    vec![
                        "udp://tracker.opentrackr.org:1337/announce".to_owned(),
                    ],
                    vec![
                        "udp://tracker.coppersurfer.tk:6969/announce"
                            .to_owned(),
                    ],
                    vec![
                        "udp://tracker.pirateparty.gr:6969/announce".to_owned(),
                    ],
                    vec!["udp://9.rarbg.to:2730/announce".to_owned()],
                    vec!["udp://9.rarbg.to:2710/announce".to_owned()],
                    vec!["udp://bt.xxx-tracker.com:2710/announce".to_owned()],
                    vec!["udp://tracker.cyberia.is:6969/announce".to_owned()],
                    vec![
                        "udp://retracker.lanta-net.ru:2710/announce".to_owned(),
                    ],
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
                    vec![
                        "udp://denis.stalker.upeer.me:6969/announce".to_owned(),
                    ],
                    vec!["udp://tracker.port443.xyz:6969/announce".to_owned()],
                    vec!["udp://tracker.moeking.me:6969/announce".to_owned()],
                    vec!["udp://exodus.desync.com:6969/announce".to_owned()],
                    vec!["udp://9.rarbg.to:2740/announce".to_owned()],
                    vec!["udp://9.rarbg.to:2720/announce".to_owned()],
                    vec!["udp://tracker.justseed.it:1337/announce".to_owned()],
                    vec![
                        "udp://tracker.torrent.eu.org:451/announce".to_owned(),
                    ],
                    vec!["udp://ipv4.tracker.harry.lu:80/announce".to_owned()],
                    vec![
                        "udp://tracker.open-internet.nl:6969/announce"
                            .to_owned(),
                    ],
                    vec!["udp://torrentclub.tech:6969/announce".to_owned()],
                    vec!["udp://open.stealth.si:80/announce".to_owned()],
                    vec!["http://tracker.tfile.co:80/announce".to_owned()],
                ]),
                info: Info {
                    source: None,
                    cross_seed_entry: None,
                    piece_length: 16_384,
                    pieces: torrent.info.pieces.clone(),
                    name: "book".to_owned(),
                    files: Some(vec![File {
                        length: 4092334,
                        path: vec!["book.pdf".to_owned()],
                    }]),
                    file_length: None,
                    metadata_size: 5095,
                    info_hash: InfoHash([
                        154, 233, 176, 64, 118, 135, 255, 199, 183, 28, 17,
                        243, 46, 47, 150, 17, 152, 204, 57, 64,
                    ]),
                },
            }
        });

        Ok(())
    }

    /// Confirm that the [`MetaInfo`] [`FromBencode`] implementation works as
    /// expected for a single-file torrent
    #[test]
    fn should_decode_single_file_torrent() -> Result<(), decoding::Error> {
        let torrent = include_bytes!("../../../test-files/debian.torrent");
        let torrent = MetaInfo::from_bencode(torrent)?;
        println!("{}", torrent.info.info_hash);

        // assert_eq!(torrent, {
        //     MetaInfo {
        //         announce: "http://bttracker.debian.org:6969/announce".to_owned(),
        //         announce_list: None,
        //         comment: Some("\"Debian CD from
        // cdimage.debian.org\"".to_owned()),         creation_date:
        // Some(1_520_682_848),         http_seeds: Some(vec![
        //             "https://cdimage.debian.org/cdimage/release/9.4.0//srv/cdbuilder.debian.org/dst/deb-cd/weekly-builds/amd64/iso-cd/debian-9.4.0-amd64-netinst.iso".to_owned(),
        //             "https://cdimage.debian.org/cdimage/archive/9.4.0//srv/cdbuilder.debian.org/dst/deb-cd/weekly-builds/amd64/iso-cd/debian-9.4.0-amd64-netinst.iso".to_owned(),
        //         ]),
        //         info: Info {
        //             source: None,
        //             cross_seed_entry: None,
        //             piece_length: 262_144,
        //             pieces:
        // include_bytes!("../../../test-files/pieces.iso").to_vec(),
        //             name: "debian-9.4.0-amd64-netinst.iso".to_owned(),
        //             files: None,
        //             file_length: Some(305_135_616),
        //             size: 23377,
        //             info_hash: InfoHash([116, 49, 169, 105, 179, 71, 225, 75,
        // 186, 100, 27, 53, 23, 192, 36, 247, 180, 13, 251, 127]),
        //         },
        //     }
        // });

        Ok(())
    }

    /// Confirm that the [`MetaInfo`] [`ToBencode`] implementation works as
    /// expected for a single-file torrent
    #[test]
    fn should_encode_single_file_torrent() -> Result<(), encoding::Error> {
        let torrent_disk = include_bytes!("../../../test-files/debian.torrent");

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
                source: None,
                cross_seed_entry: None,
                piece_length: 262_144,
                pieces: include_bytes!("../../../test-files/pieces.iso").to_vec(),
                name: "debian-9.4.0-amd64-netinst.iso".to_owned(),
                files: None,
                file_length: Some(305_135_616),
                metadata_size: 0,
                info_hash: InfoHash::default(),
            },
        };

        let data = torrent.to_bencode()?;

        assert_eq!(torrent_disk.to_vec(), data);

        Ok(())
    }

    /// Confirm that the [`metainfo::File`](struct@File) [`ToBencode`]
    /// implementation works as expected
    #[test]
    fn file_serialization() -> Result<(), encoding::Error> {
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

    /// Confirm that the [`metainfo::File`](struct@File) [`FromBencode`]
    /// implementation works as expected
    #[test]
    fn file_deserialization() -> Result<(), decoding::Error> {
        let data = b"d6:lengthi222e4:pathl1:a1:b5:c.txtee";

        let file = File::from_bencode(data)?;

        assert_eq!(
            file,
            File {
                path: ["a".to_owned(), "b".to_owned(), "c.txt".to_owned()]
                    .into(),
                length: 222,
            }
        );

        Ok(())
    }
}
