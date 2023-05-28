use speedy::{BigEndian, Readable, Writable};

use crate::error::Error;

pub(crate) const PROTOCOL_STRING: &str = "BitTorrent protocol";

#[derive(Clone, Debug, Writable, Readable)]
pub struct Handshake {
    pub pstr_len: u8,
    pub pstr: [u8; 19],
    pub reserved: [u8; 8],
    pub info_hash: [u8; 20],
    pub peer_id: [u8; 20],
}

impl Handshake {
    pub fn new(info_hash: [u8; 20], peer_id: [u8; 20]) -> Self {
        Self {
            pstr_len: u8::to_be(19),
            pstr: b"BitTorrent protocol".to_owned(),
            reserved: [0u8; 8],
            info_hash,
            peer_id,
        }
    }
    pub fn serialize(&self) -> Result<Vec<u8>, Error> {
        self.write_to_vec_with_ctx(BigEndian {})
            .map_err(Error::SpeedyError)
    }
    pub fn deserialize(buf: &[u8]) -> Self {
        Self::read_from_buffer_with_ctx(BigEndian {}, buf).unwrap()
    }
    pub fn validate(&self, target: Self) -> bool {
        if target.peer_id.len() != 20 {
            eprintln!("-- warning -- invalid peer_id from receiving handshake");
            return false;
        }
        if self.info_hash != self.info_hash {
            eprintln!("-- warning -- info_hash from receiving handshake does not match ours");
            return false;
        }
        if target.pstr_len != 19 {
            eprintln!("-- warning -- handshake with wrong pstr_len, dropping connection");
            return false;
        }
        if target.pstr != b"BitTorrent protocol".to_owned() {
            eprintln!("-- warning -- handshake with wrong pstr, dropping connection");
            return false;
        }
        true
    }
}

//
//  All of the remaining messages in the protocol
//  take the form of: (except for a few)
//  <length prefix><message ID><payload>.
//  The length prefix is a four byte big-endian value.
//  The message ID is a single decimal byte.
//  The payload is message dependent.
//

#[derive(Clone, Debug, Writable, Readable)]
pub struct KeepAlive {
    pub len: u32,
}

impl KeepAlive {
    pub fn serialize(&self) -> Result<Vec<u8>, Error> {
        self.write_to_vec_with_ctx(BigEndian {})
            .map_err(Error::SpeedyError)
    }
    pub fn deserialize(buf: &[u8]) -> Result<Self, Error> {
        Self::read_from_buffer_with_ctx(BigEndian {}, buf).map_err(Error::SpeedyError)
    }
    pub fn new() -> Self {
        Self {
            len: u32::to_be(0000),
        }
    }
}

#[derive(Clone, Debug, Writable, Readable)]
pub struct Choke {
    pub len: u32,
    pub id: u8,
}

impl Choke {
    pub fn serialize(&self) -> Result<Vec<u8>, Error> {
        self.write_to_vec_with_ctx(BigEndian {})
            .map_err(Error::SpeedyError)
    }
    pub fn deserialize(buf: &[u8]) -> Result<Self, Error> {
        Self::read_from_buffer_with_ctx(BigEndian {}, buf).map_err(Error::SpeedyError)
    }
    pub fn new() -> Self {
        Self {
            len: u32::to_be(0001),
            id: u8::to_be(0),
        }
    }
}

#[derive(Clone, Debug, Writable, Readable)]
pub struct Unchoke {
    pub len: u32,
    pub id: u8,
}

impl Unchoke {
    pub fn serialize(&self) -> Result<Vec<u8>, Error> {
        self.write_to_vec_with_ctx(BigEndian {})
            .map_err(Error::SpeedyError)
    }
    pub fn deserialize(buf: &[u8]) -> Result<Self, Error> {
        Self::read_from_buffer_with_ctx(BigEndian {}, buf).map_err(Error::SpeedyError)
    }
    pub fn new() -> Self {
        Self {
            len: u32::to_be(0001),
            id: u8::to_be(1),
        }
    }
}

#[derive(Clone, Debug, Writable, Readable)]
pub struct Interested {
    pub len: u32,
    pub id: u8,
}

impl Interested {
    pub fn serialize(&self) -> Result<Vec<u8>, Error> {
        self.write_to_vec_with_ctx(BigEndian {})
            .map_err(Error::SpeedyError)
    }
    pub fn deserialize(buf: &[u8]) -> Result<Self, Error> {
        Self::read_from_buffer_with_ctx(BigEndian {}, buf).map_err(Error::SpeedyError)
    }
    pub fn new() -> Self {
        Self {
            len: u32::to_be(0001),
            id: u8::to_be(2),
        }
    }
}

#[derive(Clone, Debug, Writable, Readable)]
pub struct NotInterested {
    pub len: u32,
    pub id: u8,
}

impl NotInterested {
    pub fn serialize(&self) -> Result<Vec<u8>, Error> {
        self.write_to_vec_with_ctx(BigEndian {})
            .map_err(Error::SpeedyError)
    }
    pub fn deserialize(buf: &[u8]) -> Result<Self, Error> {
        Self::read_from_buffer_with_ctx(BigEndian {}, buf).map_err(Error::SpeedyError)
    }
    pub fn new() -> Self {
        Self {
            len: u32::to_be(0001),
            id: u8::to_be(3),
        }
    }
}

#[derive(Clone, Debug, Writable, Readable)]
pub struct Have {
    pub len: u32,
    pub id: u8,
    pub piece_index: u32,
}

impl Have {
    pub fn serialize(&self) -> Result<Vec<u8>, Error> {
        self.write_to_vec_with_ctx(BigEndian {})
            .map_err(Error::SpeedyError)
    }
    pub fn deserialize(buf: &[u8]) -> Result<Self, Error> {
        Self::read_from_buffer_with_ctx(BigEndian {}, buf).map_err(Error::SpeedyError)
    }
    pub fn new() -> Self {
        Self {
            len: u32::to_be(0005),
            id: u8::to_be(4),
            piece_index: u32::to_be(4),
        }
    }
}

///
/// bitfield: <len=0001+X><id=5><bitfield>
///
#[derive(Clone, Debug, Writable, Readable)]
pub struct Bitfield {
    pub len: u32,
    pub id: u8,
    pub bitfield: Vec<u8>,
}

impl Bitfield {
    pub fn serialize(&self) -> Result<Vec<u8>, Error> {
        self.write_to_vec_with_ctx(BigEndian {})
            .map_err(Error::SpeedyError)
    }
    pub fn deserialize(buf: &[u8]) -> Result<Self, Error> {
        Self::read_from_buffer_with_ctx(BigEndian {}, buf).map_err(Error::SpeedyError)
    }
    pub fn new(msg_len: u32) -> Self {
        let bitfield = vec![0u8; msg_len as usize];

        Self {
            len: u32::to_be(0001 + bitfield.len() as u32),
            id: u8::to_be(5),
            bitfield,
        }
    }
}

#[derive(Clone, Debug, Writable, Readable)]
pub struct Request {
    pub len: u32,
    pub id: u8,
    pub index: u32,
    pub begin: u32,
    pub length: u32,
}

impl Request {
    pub fn serialize(&self) -> Result<Vec<u8>, Error> {
        self.write_to_vec_with_ctx(BigEndian {})
            .map_err(Error::SpeedyError)
    }
    pub fn deserialize(buf: &[u8]) -> Result<Self, Error> {
        Self::read_from_buffer_with_ctx(BigEndian {}, buf).map_err(Error::SpeedyError)
    }
    pub fn new(index: u32, begin: u32, length: u32) -> Self {
        Self {
            len: u32::to_be(0013),
            id: u8::to_be(6),
            index,
            begin,
            length,
        }
    }
}

#[derive(Clone, Debug, Writable, Readable)]
pub struct Piece {
    pub len: u32,
    pub id: u8,
    pub index: u32,
    pub begin: u32,
    pub block: Vec<u8>,
}

impl Piece {
    pub fn serialize(&self) -> Result<Vec<u8>, Error> {
        self.write_to_vec_with_ctx(BigEndian {})
            .map_err(Error::SpeedyError)
    }
    pub fn deserialize(buf: &[u8]) -> Result<Self, Error> {
        Self::read_from_buffer_with_ctx(BigEndian {}, buf).map_err(Error::SpeedyError)
    }
    pub fn new(index: u32, begin: u32, block: Vec<u8>) -> Self {
        Self {
            len: u32::to_be(0009 + block.len() as u32),
            id: u8::to_be(7),
            index,
            begin,
            block,
        }
    }
}

#[derive(Clone, Debug, Writable, Readable)]
pub struct Cancel {
    pub len: u32,
    pub id: u8,
    pub index: u32,
    pub begin: u32,
    pub length: u32,
}

impl Cancel {
    pub fn serialize(&self) -> Result<Vec<u8>, Error> {
        self.write_to_vec_with_ctx(BigEndian {})
            .map_err(Error::SpeedyError)
    }
    pub fn deserialize(buf: &[u8]) -> Result<Self, Error> {
        Self::read_from_buffer_with_ctx(BigEndian {}, buf).map_err(Error::SpeedyError)
    }
    pub fn new(index: u32, begin: u32, length: u32) -> Self {
        Self {
            len: u32::to_be(0013),
            id: u8::to_be(8),
            index,
            begin,
            length,
        }
    }
}

#[derive(Clone, Debug, Writable, Readable)]
pub struct Port {
    pub len: u32,
    pub id: u8,
    pub listen_port: u16,
}

impl Port {
    pub fn serialize(&self) -> Result<Vec<u8>, Error> {
        self.write_to_vec_with_ctx(BigEndian {})
            .map_err(Error::SpeedyError)
    }
    pub fn deserialize(buf: &[u8]) -> Result<Self, Error> {
        Self::read_from_buffer_with_ctx(BigEndian {}, buf).map_err(Error::SpeedyError)
    }
    pub fn new(listen_port: u16) -> Self {
        Self {
            len: u32::to_be(0003),
            id: u8::to_be(9),
            listen_port,
        }
    }
}
