//! Metainfo is a .torrent file with information about the Torrent.
//! From the magnet link, we get the Metainfo from other peers.
use std::collections::BTreeMap;

use bendy::{
    decoding::{self, FromBencode, Object, ResultExt},
    encoding::{self, AsString, Error, SingleItemEncoder, ToBencode},
};
use tracing::warn;

use crate::{
    error,
    extensions::core::{BlockInfo, BLOCK_LEN},
};

/// Metainfo is a .torrent file with information about the Torrent.
/// From the magnet link, we get the Metainfo from other peers.
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
/// <https://fileformats.fandom.com/wiki/Torrent_file>
/// in a multi file format, `name` is name of the directory
/// `file_length` is specific to Single File format
/// in a multi file format, `file_length` is replaced to `files`
#[derive(Debug, PartialEq, Clone, Default)]
pub struct Info {
    /// piece length - number of bytes in a piece
    pub piece_length: u32,
    /// A (byte) string consisting of the concatenation of all 20-byte SHA1
    /// hash values, one per piece.
    pub pieces: Vec<u8>,
    /// name of the file
    pub name: String,
    /// length - bytes of the entire file
    pub file_length: Option<u32>,
    pub files: Option<Vec<File>>,
    pub metadata_size: Option<u64>,
}

impl Info {
    /// Size of the entire Info file.
    pub fn metadata_size(&self) -> Result<u64, Error> {
        self.to_bencode().map(|v| v.len() as u64)
    }
    pub fn name(mut self, name: String) -> Self {
        self.name = name;
        self
    }
    /// Calculate how many pieces there are.
    pub fn pieces(&self) -> u32 {
        (self.pieces.len() as u32).div_ceil(20)
    }
    /// Calculate how many blocks there are in the entire torrent.
    pub fn blocks_len(&self) -> u32 {
        self.blocks_per_piece() * (self.pieces.len() as u32).div_ceil(20)
    }
    /// Calculate how many blocks there are per piece
    pub fn blocks_per_piece(&self) -> u32 {
        self.piece_length / BLOCK_LEN
    }
    /// Get all block_infos of a torrent
    /// Returns an Err if the Info is malformed, if it does not have `files` or
    /// `file_length`.
    pub fn get_block_infos(
        &self,
    ) -> Result<BTreeMap<usize, Vec<BlockInfo>>, error::Error> {
        let total_size = self.get_size() as u32;
        let mut block_infos: BTreeMap<usize, Vec<BlockInfo>> = BTreeMap::new();
        let mut processed_bytes = 0;
        let mut offset_within_file = 0;
        let mut file_index = 0;

        while processed_bytes < total_size {
            let remaining_in_file =
                self.files.as_ref().map_or(total_size - processed_bytes, |f| {
                    f[file_index].length - offset_within_file
                });
            let len = [
                remaining_in_file,
                self.piece_length - processed_bytes % self.piece_length,
                BLOCK_LEN,
            ]
            .iter()
            .cloned()
            .min()
            .unwrap();

            let entry = block_infos
                .entry((processed_bytes / self.piece_length) as usize)
                .or_default();

            entry.push(BlockInfo {
                index: (processed_bytes / self.piece_length),
                begin: processed_bytes % self.piece_length,
                len,
            });

            processed_bytes += len;
            offset_within_file += len;

            if let Some(files) = &self.files {
                while file_index < files.len()
                    && offset_within_file >= files[file_index].length
                {
                    offset_within_file -= files[file_index].length;
                    file_index += 1;
                }
            }
        }

        Ok(block_infos.into())
    }
    /// Get the size in bytes of the files of the torrent.
    pub fn get_size(&self) -> u64 {
        // multi file torrent
        if let Some(files) = &self.files {
            return files.iter().fold(0, |acc, x| acc + x.length as u64);
        }

        // single file torrent
        if let Some(f) = self.file_length {
            return f as u64;
        }

        warn!("tried to call get_size of malformed Info {self:#?}");
        0
    }

    /// Get the size (in bytes) of a piece.
    pub fn piece_size(&self, piece_index: usize) -> u32 {
        let total_size = self.get_size() as u32;
        if piece_index == self.pieces() as usize - 1 {
            let remainder = total_size % self.piece_length;
            if remainder == 0 {
                self.piece_length
            } else {
                remainder
            }
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
    pub length: u32,
    /// Path of the file, excluding the parent name.
    pub path: Vec<String>,
}

impl File {
    /// Get the len of the given piece in the file, in bytes..
    pub fn get_piece_len(&self, piece: u32, piece_length: u32) -> u32 {
        let b = (piece * piece_length) + piece_length;
        if b <= self.length {
            piece_length
        } else {
            self.length % piece_length
        }
    }
    /// Return the number of pieces in the file, rounded up.
    pub fn pieces(&self, piece_length: u32) -> u32 {
        let pieces = self.length as f32 / piece_length as f32;
        pieces.ceil() as u32
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
                    length =
                        u32::decode_bencode_object(value).context("length")?;
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

        let name =
            name.ok_or_else(|| decoding::Error::missing_field("name"))?;
        let piece_length = piece_length
            .ok_or_else(|| decoding::Error::missing_field("piece_length"))?;
        let pieces =
            pieces.ok_or_else(|| decoding::Error::missing_field("pieces"))?;

        // Check that we discovered all necessary fields
        Ok(Info {
            files,
            file_length,
            name,
            piece_length,
            pieces,
            metadata_size: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// piece_length: 15
    /// -------------------
    /// | f: 30           |
    /// ---------p---------
    /// | b: 15  | b: 15  |
    /// -------------------
    #[test]
    fn get_block_infos_smaller_than_block_info() {
        let info = Info {
            file_length: Some(30),
            piece_length: 15,
            pieces: vec![0; 40],
            ..Default::default()
        };
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
        let info = Info {
            file_length: Some(32868),
            piece_length: BLOCK_LEN,
            pieces: vec![0; 60],
            ..Default::default()
        };
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
        let info = Info {
            files: Some(vec![File {
                length: 32768,
                path: vec!["a.txt".to_owned()],
            }]),
            piece_length: BLOCK_LEN,
            pieces: vec![0; 40],
            ..Default::default()
        };
        let blocks = info.get_block_infos().unwrap();
        assert_eq!(
            *blocks.get(&0).unwrap(),
            vec![BlockInfo { index: 0, begin: 0, len: BLOCK_LEN },]
        );
        assert_eq!(
            *blocks.get(&2).unwrap(),
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
        let info = Info {
            files: Some(vec![File {
                length: 32768,
                path: vec!["a.txt".to_owned()],
            }]),
            piece_length: 32668,
            pieces: vec![0; 40],
            ..Default::default()
        };
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

    /// piece_length: 45152
    /// ---------------------------------------
    /// | f: 16384   | f: 12384 | f: 16384    |
    /// p------------f----------f-------------|
    /// | b: 16384   | b: 12384 | b: 16384    |
    /// --------------------------------------|
    #[test]
    fn get_block_infos_file_boundary() {
        let info = Info {
            files: Some(vec![
                File { length: BLOCK_LEN, path: vec!["a.txt".to_owned()] },
                File { length: 12384, path: vec!["b.txt".to_owned()] },
                File { length: BLOCK_LEN, path: vec!["c.txt".to_owned()] },
            ]),
            piece_length: 45152,
            pieces: vec![0; 20],
            ..Default::default()
        };
        let blocks = info.get_block_infos().unwrap();

        // check the pieces size
        assert_eq!(info.piece_size(0), 45152);

        assert_eq!(
            *blocks.get(&0).unwrap(),
            vec![
                BlockInfo { index: 0, begin: 0, len: BLOCK_LEN },
                BlockInfo { index: 0, begin: BLOCK_LEN, len: 12384 },
                BlockInfo {
                    index: 0,
                    begin: BLOCK_LEN + 12384,
                    len: BLOCK_LEN,
                },
            ]
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
        let info = Info {
            files: Some(vec![
                File { length: 10, path: vec!["".to_owned()] },
                File {
                    length: 32768, // 2 blocks
                    path: vec!["".to_owned()],
                },
            ]),
            piece_length: 32668, // -100 of block_len
            pieces: vec![0; 40],
            ..Default::default()
        };
        let blocks = info.get_block_infos().unwrap();

        // check the pieces size
        assert_eq!(info.piece_size(0), 32668);
        assert_eq!(info.piece_size(1), 110);

        assert_eq!(
            *blocks.get(&0).unwrap(),
            vec![
                BlockInfo { index: 0, begin: 0, len: 10 },
                BlockInfo { index: 0, begin: 10, len: BLOCK_LEN },
                BlockInfo { index: 0, begin: 10 + BLOCK_LEN, len: 16274 },
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
        let info = Info {
            files: Some(vec![
                File {
                    length: 32768, // 2 blocks
                    path: vec!["".to_owned()],
                },
                File { length: 10, path: vec!["".to_owned()] },
            ]),
            piece_length: 32668, // -100 of block_len
            pieces: vec![0u8; 40],
            ..Default::default()
        };

        let blocks = info.get_block_infos().unwrap();

        // check the pieces size
        assert_eq!(info.piece_size(0), 32668);
        assert_eq!(info.piece_size(1), 110);

        assert_eq!(
            *blocks.get(&0).unwrap(),
            Vec::from([
                BlockInfo { index: 0, begin: 0, len: BLOCK_LEN },
                BlockInfo { index: 0, begin: BLOCK_LEN, len: 16284 },
            ])
        );

        assert_eq!(
            *blocks.get(&1).unwrap(),
            Vec::from([
                BlockInfo { index: 1, begin: 0, len: 100 },
                BlockInfo { index: 1, begin: 100, len: 10 },
            ])
        );
    }

    // #[test]
    // fn utility_functions_complex_multi() -> Result<(), Error> {
    //     //
    //     // Complex multi file torrent, 64 blocks per piece
    //     //
    //     let metainfo = include_bytes!("../../../test-files/music.torrent");
    //     let metainfo = MetaInfo::from_bencode(metainfo).unwrap();
    //     let info = metainfo.info;
    //
    //     let bi = info.get_block_infos().unwrap();
    //     let mut x = info.get_block_infos().unwrap();
    //     x.pop_back();
    //
    //     let size = info.get_size();
    //     let per_piece = info.blocks_per_piece();
    //     let pieces = info.pieces();
    //     let info_bytes = bi.iter().fold(0, |acc, x| acc + x.len);
    //     println!("--- info ---");
    //     println!("size {size}");
    //     println!("piece_len {}", info.piece_length);
    //     println!("per_piece {per_piece}");
    //     println!("pieces {pieces}");
    //     println!("blocks {}", pieces * per_piece);
    //
    //     // validations on the entire info
    //     assert_eq!(info_bytes as u64, 863104781);
    //     assert_eq!(863104781, size);
    //     assert_eq!(64, per_piece);
    //     assert_eq!(824, pieces);
    //
    //     let files = info.files.clone().unwrap();
    //
    //     let file0 = &files[0];
    //
    //     let pieces_file = file0.pieces(info.piece_length);
    //     let blocks_file = pieces_file * per_piece;
    //
    //     // file0 has 25 pieces (0 idx based) with 1611 blocks
    //     println!("--- file[0] ---");
    //     println!("{:#?}", files[0]);
    //
    //     // 611
    //     // assert_eq!(1664, blocks_file);
    //     assert_eq!(26, pieces_file);
    //     assert_eq!(26384160, file0.length);
    //
    //     println!("blocks {:#?}", blocks_file);
    //     println!("pieces {:#?}", pieces_file);
    //     println!("blocks per piece {:#?}", pieces_file);
    //
    //     let block = bi.get(0).unwrap();
    //     // println!("--- piece 0, block 0 (first) ---");
    //     assert_eq!(*block, BlockInfo { index: 0, begin: 0, len: BLOCK_LEN });
    //
    //     let block = bi.get(1).unwrap();
    //     // println!("--- piece 0, block 1 (second) ---");
    //     assert_eq!(
    //         *block,
    //         BlockInfo { index: 0, begin: BLOCK_LEN, len: BLOCK_LEN }
    //     );
    //
    //     let block = bi.get(2).unwrap();
    //     // println!("--- piece 0, block 2 (third) ---");
    //     assert_eq!(
    //         *block,
    //         BlockInfo { index: 0, begin: BLOCK_LEN * 2, len: BLOCK_LEN }
    //     );
    //
    //     let block = bi.get(63).unwrap();
    //     println!("--- piece 0, block 63 ---");
    //     println!("{block:#?}");
    //
    //     let block = bi.get(64).unwrap();
    //     // println!("--- piece 0, block 64 ---");
    //     // println!("{block:#?}");
    //     assert_eq!(*block, BlockInfo { index: 1, begin: 0, len: 16384 });
    //     let block = bi.get(1608).unwrap();
    //     println!("--- piece 25, block 1608 before before last ---");
    //     println!("{block:#?}");
    //     // 10 blocks on the last piece of this file
    //     let block = bi.get(1609).unwrap();
    //     println!("--- piece 25, block 1609 ---");
    //     println!("{block:#?}");
    //     assert_eq!(*block, BlockInfo { index: 25, begin: 147456, len: 16384 });
    //     // last block of the last piece of this file
    //     let block = bi.get(1610).unwrap();
    //     println!("--- piece 25, block 1610 (last of file) ---");
    //     println!("{block:#?}");
    //
    //     let bytes_so_far = bi.iter().take(1611).fold(0, |acc, x| acc + x.len);
    //     assert_eq!(bytes_so_far, file0.length);
    //
    //     assert_eq!(*block, BlockInfo { index: 25, begin: 163840, len: 5920 });
    //     // file0 ended /\
    //
    //     let file1 = &files[1];
    //
    //     let pieces_file = file1.pieces(info.piece_length);
    //     let blocks_file = pieces_file * per_piece;
    //     println!("--- file[1] ---");
    //     println!("{file1:#?}");
    //
    //     // validations on file[1]
    //     assert_eq!(512, blocks_file);
    //     assert_eq!(8, pieces_file);
    //     assert_eq!(8281625, file1.length);
    //
    //     // file1 has 8 pieces (0 idx based)
    //     // 507
    //     println!("pieces {pieces_file}");
    //     println!("blocks {blocks_file}");
    //
    //     let block = bi.get(1611).unwrap();
    //     println!("--- file1 piece 25, block 1611 (first of file) ---");
    //     println!("{block:#?}");
    //     assert_eq!(
    //         *block,
    //         BlockInfo { index: 25, begin: 169760, len: BLOCK_LEN }
    //     );
    //
    //     // 506 blocks
    //     let block = bi.get(1610 + 506).unwrap();
    //     let bytes_so_far =
    //         bi.iter().take(1610 + 507).fold(0, |acc, x| acc + x.len);
    //     println!("--- file1 piece 25, block 1611 (last of file) ---");
    //     println!("bytes_so_far {bytes_so_far}");
    //     println!("{block:#?}");
    //     assert_eq!(*block, BlockInfo { index: 33, begin: 49152, len: 13625 });
    //
    //     Ok(())
    // }
    //
    // #[tokio::test]
    // async fn utility_functions_simple_multi() -> Result<(), Error> {
    //     //
    //     // Simple multi file torrent, 1 block per piece
    //     //
    //     let metainfo = include_bytes!("../../../test-files/book.torrent");
    //     let metainfo = MetaInfo::from_bencode(metainfo).unwrap();
    //     let info = metainfo.info;
    //
    //     let bi = info.get_block_infos().unwrap();
    //     let size = info.get_size();
    //     let per_piece = info.blocks_per_piece();
    //     let pieces = info.pieces();
    //
    //     let files = info.files.unwrap();
    //     let files_bytes = files.iter().fold(0, |acc, x| acc + x.length);
    //
    //     let file = &files[0];
    //     let pieces_file = file.length as f32 / info.piece_length as f32;
    //     let pieces_file = pieces_file.ceil() as u32;
    //     let blocks_file = pieces_file * per_piece;
    //
    //     // get total size of file from block_infos
    //     let total_bis = bi.iter().fold(0, |acc, x| acc + x[0].len);
    //
    //     println!("--- file[0] ---");
    //     println!("{:#?}", files[0]);
    //     println!("blocks_file {blocks_file:#?}");
    //     println!("piece_len {:#?}", info.piece_length);
    //     println!("pieces in file {pieces_file:#?}");
    //
    //     assert_eq!(total_bis, 4092334);
    //     assert_eq!(bi.len(), 250);
    //     assert_eq!(size, 4092334);
    //     assert_eq!(files_bytes, 4092334);
    //     assert_eq!(per_piece, 1);
    //     assert_eq!(pieces, 250);
    //
    //     let block = bi.get(&0).unwrap();
    //     println!("--- piece 0, block 0 (only one) ---");
    //     println!("{block:#?}");
    //     assert_eq!(block[0], BlockInfo { index: 0, begin: 0, len: BLOCK_LEN });
    //
    //     let block = bi.get(&1).unwrap();
    //     println!("--- piece 1, block 1 (only one) ---");
    //     println!("{block:#?}");
    //     assert_eq!(block[0], BlockInfo { index: 1, begin: 0, len: BLOCK_LEN });
    //
    //     let block = bi.get(&249).unwrap();
    //     println!("--- piece 249, block 249 (last, only one) ---");
    //     println!("{block:#?}");
    //     assert_eq!(block[0], BlockInfo { index: 249, begin: 0, len: 12718 });
    //
    //     Ok(())
    // }
    //
    // #[tokio::test]
    // async fn utility_functions_complex_single() -> Result<(), Error> {
    //     //
    //     // Simple multi file torrent, 16 blocks per piece
    //     //
    //     let torrent_debian_bytes =
    //         include_bytes!("../../../test-files/debian.torrent");
    //     let torrent = MetaInfo::from_bencode(torrent_debian_bytes).unwrap();
    //     let info = torrent.info;
    //     let bi = info.get_block_infos().unwrap();
    //     let size = info.get_size();
    //     let per_piece = info.blocks_per_piece();
    //     let pieces = info.pieces();
    //
    //     let pieces_file = size as f32 / info.piece_length as f32;
    //     let pieces_file = pieces_file.ceil() as u32;
    //     let blocks_file = pieces_file * per_piece;
    //
    //     // get total size of file from block_infos
    //     let total_bis = bi.iter().fold(0, |acc, x| acc + x.len);
    //
    //     println!("--- file ---");
    //     println!("file_length {size:#?}");
    //     println!("blocks_file {blocks_file:#?}");
    //     println!("blocks per piece {per_piece:#?}");
    //     println!("piece_len {:#?}", info.piece_length);
    //     println!("pieces in file {pieces_file:#?}");
    //
    //     assert_eq!(total_bis, 305135616);
    //     assert_eq!(total_bis as u64, size);
    //     assert_eq!(bi.len(), 18624);
    //     assert_eq!(size, 305135616);
    //     assert_eq!(per_piece, 16);
    //     assert_eq!(pieces, 1164);
    //
    //     let block = bi.get(0).unwrap();
    //     println!("--- piece 0, block 0 (first) ---");
    //     println!("{block:#?}");
    //     assert_eq!(*block, BlockInfo { index: 0, begin: 0, len: BLOCK_LEN });
    //
    //     let block = bi.get(1).unwrap();
    //     println!("--- piece 0, block 1 ---");
    //     println!("{block:#?}");
    //     assert_eq!(
    //         *block,
    //         BlockInfo { index: 0, begin: BLOCK_LEN, len: BLOCK_LEN }
    //     );
    //
    //     let block = bi.get(2).unwrap();
    //     println!("--- piece 0, block 2 ---");
    //     println!("{block:#?}");
    //     assert_eq!(
    //         *block,
    //         BlockInfo { index: 0, begin: BLOCK_LEN * 2, len: BLOCK_LEN }
    //     );
    //
    //     let block = bi.get(18623).unwrap();
    //     println!("--- piece 1163, block 18623 (last) ---");
    //     println!("{block:#?}");
    //     assert_eq!(
    //         *block,
    //         BlockInfo { index: 1163, begin: 245760, len: BLOCK_LEN }
    //     );
    //
    //     Ok(())
    // }
    //
    // #[test]
    // fn get_block_infos_long_torrent() {
    //     let info = Info {
    //         piece_length: 32768,
    //         pieces: vec![0; 5480], // 274 pieces * 20 bytes each
    //         name: "name".to_string(),
    //         file_length: None,
    //         metadata_size: None,
    //         files: Some(vec![
    //             // 308 blocks
    //             File {
    //                 length: 5034059,
    //                 path: vec!["dir".to_string(), "file_a.pdf".to_string()],
    //             },
    //             // 1 block
    //             File { length: 62, path: vec!["file_1.txt".to_string()] },
    //             // 1 block
    //             File { length: 237, path: vec!["file_2.txt".to_string()] },
    //         ]),
    //     };
    //
    //     let block_infos = info.get_block_infos().unwrap();
    //
    //     let last_first_file = &block_infos[307];
    //     let last_second_file = &block_infos[308];
    //     let last_third_file = &block_infos[309];
    //
    //     let last = block_infos.back().unwrap();
    //     let len = block_infos.len(); // 306 blocks
    //
    //     println!("last {last:#?}");
    //     println!("len {len:#?}");
    //
    //     assert_eq!(
    //         *last_first_file,
    //         BlockInfo { index: 153, begin: 16384, len: 4171 }
    //     );
    //
    //     assert_eq!(
    //         *last_second_file,
    //         BlockInfo { index: 153, begin: 20555, len: 62 }
    //     );
    //
    //     assert_eq!(
    //         *last_third_file,
    //         BlockInfo { index: 153, begin: 20617, len: 237 }
    //     );
    // }
    //
    // /// Confirm that the [`MetaInfo`] [`ToBencode`] and [`FromBencode`]
    // /// implementations work as expected for a multi-file torrent
    // #[test]
    // fn should_encode_multi_file_torrent() -> Result<(), encoding::Error> {
    //     let torrent_book_bytes =
    //         include_bytes!("../../../test-files/book.torrent");
    //
    //     let torrent = MetaInfo::from_bencode(torrent_book_bytes).unwrap();
    //     let torrent_bytes = torrent.to_bencode().unwrap();
    //     let torrent = MetaInfo::from_bencode(&torrent_bytes).unwrap();
    //
    //     assert_eq!(torrent_bytes, torrent.to_bencode()?);
    //
    //     Ok(())
    // }

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
                    vec!["udp://tracker.leechers-paradise.org:6969/announce"
                        .to_owned()],
                    vec!["udp://tracker.internetwarriors.net:1337/announce"
                        .to_owned()],
                    vec![
                        "udp://tracker.opentrackr.org:1337/announce".to_owned()
                    ],
                    vec!["udp://tracker.coppersurfer.tk:6969/announce"
                        .to_owned()],
                    vec![
                        "udp://tracker.pirateparty.gr:6969/announce".to_owned()
                    ],
                    vec!["udp://9.rarbg.to:2730/announce".to_owned()],
                    vec!["udp://9.rarbg.to:2710/announce".to_owned()],
                    vec!["udp://bt.xxx-tracker.com:2710/announce".to_owned()],
                    vec!["udp://tracker.cyberia.is:6969/announce".to_owned()],
                    vec![
                        "udp://retracker.lanta-net.ru:2710/announce".to_owned()
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
                        "udp://denis.stalker.upeer.me:6969/announce".to_owned()
                    ],
                    vec!["udp://tracker.port443.xyz:6969/announce".to_owned()],
                    vec!["udp://tracker.moeking.me:6969/announce".to_owned()],
                    vec!["udp://exodus.desync.com:6969/announce".to_owned()],
                    vec!["udp://9.rarbg.to:2740/announce".to_owned()],
                    vec!["udp://9.rarbg.to:2720/announce".to_owned()],
                    vec!["udp://tracker.justseed.it:1337/announce".to_owned()],
                    vec!["udp://tracker.torrent.eu.org:451/announce".to_owned()],
                    vec!["udp://ipv4.tracker.harry.lu:80/announce".to_owned()],
                    vec!["udp://tracker.open-internet.nl:6969/announce"
                        .to_owned()],
                    vec!["udp://torrentclub.tech:6969/announce".to_owned()],
                    vec!["udp://open.stealth.si:80/announce".to_owned()],
                    vec!["http://tracker.tfile.co:80/announce".to_owned()],
                ]),
                info: Info {
                    piece_length: 16_384,
                    metadata_size: None,
                    pieces: torrent.info.pieces.clone(),
                    name: "book".to_owned(),
                    files: Some(vec![File {
                        length: 4092334,
                        path: vec!["book.pdf".to_owned()],
                    }]),
                    file_length: None,
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
                    metadata_size: None,
                    piece_length: 262_144,
                    pieces: include_bytes!("../../../test-files/pieces.iso").to_vec(),
                    name: "debian-9.4.0-amd64-netinst.iso".to_owned(),
                    files: None,
                    file_length: Some(305_135_616),
                },
            }
        });

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
                metadata_size: None,
                piece_length: 262_144,
                pieces: include_bytes!("../../../test-files/pieces.iso").to_vec(),
                name: "debian-9.4.0-amd64-netinst.iso".to_owned(),
                files: None,
                file_length: Some(305_135_616),
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
