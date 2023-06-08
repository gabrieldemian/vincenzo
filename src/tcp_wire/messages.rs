use bytes::{Buf, BufMut, BytesMut};
use log::warn;
use speedy::{BigEndian, Readable, Writable};
use std::io::Cursor;
use tokio::io;
use tokio_util::codec::{Decoder, Encoder};

use crate::{bitfield::Bitfield, error::Error, tcp_wire::lib::Block};

use super::lib::BlockInfo;

pub enum Message {
    Handshake {
        pstr_len: u8,
        pstr: [u8; 19],
        reserved: [u8; 8],
        info_hash: [u8; 20],
        peer_id: [u8; 20],
    },
    KeepAlive,
    Bitfield(Bitfield),
    Choke,
    Unchoke,
    Interested,
    NotInterested,
    Have {
        piece_index: usize,
    },
    Request(BlockInfo),
    Piece(Block),
    Cancel(BlockInfo),
}

#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum MessageId {
    Choke = 0,
    Unchoke = 1,
    Interested = 2,
    NotInterested = 3,
    Have = 4,
    Bitfield = 5,
    Request = 6,
    Piece = 7,
    Cancel = 8,
    Handshake = 84,
}

impl TryFrom<u8> for MessageId {
    type Error = io::Error;

    fn try_from(k: u8) -> Result<Self, Self::Error> {
        use MessageId::*;
        match k {
            k if k == Choke as u8 => Ok(Choke),
            k if k == Unchoke as u8 => Ok(Unchoke),
            k if k == Interested as u8 => Ok(Interested),
            k if k == NotInterested as u8 => Ok(NotInterested),
            k if k == Have as u8 => Ok(Have),
            k if k == Bitfield as u8 => Ok(Bitfield),
            k if k == Request as u8 => Ok(Request),
            k if k == Piece as u8 => Ok(Piece),
            k if k == Cancel as u8 => Ok(Cancel),
            k if k == Handshake as u8 => Ok(Handshake),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Unknown message id",
            )),
        }
    }
}

pub struct PeerCodec;

// Encode bytes into messages
impl Encoder<Message> for PeerCodec {
    type Error = io::Error;

    fn encode(&mut self, item: Message, buf: &mut BytesMut) -> Result<(), Self::Error> {
        match item {
            Message::Handshake {
                info_hash, peer_id, ..
            } => {
                buf.put_u8(19);
                buf.extend_from_slice(b"BitTorrent protocol");
                buf.extend_from_slice(&[0u8; 8]);
                buf.extend_from_slice(&info_hash);
                buf.extend_from_slice(&peer_id);
            }
            Message::KeepAlive => {
                buf.put_u32(0);
            }
            Message::Bitfield(bitfield) => {
                buf.put_u32(1 + bitfield.len_bytes() as u32);
                buf.put_u8(MessageId::Bitfield as u8);
                buf.extend_from_slice(bitfield.inner.as_slice());
            }
            Message::Choke => {
                buf.put_u32(1);
                buf.put_u8(MessageId::Choke as u8);
            }
            Message::Unchoke => {
                buf.put_u32(1);
                buf.put_u8(MessageId::Unchoke as u8);
            }
            Message::Interested => {
                let msg_len = 1;
                buf.put_u32(msg_len);
                buf.put_u8(MessageId::Interested as u8);
            }
            Message::NotInterested => {
                let msg_len = 1;
                buf.put_u32(msg_len);
                buf.put_u8(MessageId::NotInterested as u8);
            }
            Message::Have { piece_index } => {
                let msg_len = 1 + 4;
                buf.put_u32(msg_len);
                buf.put_u8(MessageId::Have as u8);
                let piece_index = piece_index
                    .try_into()
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
                buf.put_u32(piece_index);
            }
            Message::Request(block) => {
                let msg_len = 1 + 4 + 4 + 4;
                buf.put_u32(msg_len);
                buf.put_u8(MessageId::Request as u8);
                block.encode(buf)?;
            }
            Message::Piece(block) => {
                let Block {
                    index,
                    begin,
                    block,
                } = block;

                let msg_len = 1 + 4 + 4 + block.len() as u32;

                buf.put_u32(msg_len);
                buf.put_u8(MessageId::Piece as u8);

                let index = index
                    .try_into()
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;

                buf.put_u32(index);
                buf.put_u32(begin);
                buf.put(&block[..]);
            }
            Message::Cancel(block) => {
                let msg_len = 1 + 4 + 4 + 4;

                buf.put_u32(msg_len);
                buf.put_u8(MessageId::Cancel as u8);

                block.encode(buf)?;
            }
        }
        Ok(())
    }
}

impl Decoder for PeerCodec {
    type Item = Message;
    type Error = io::Error;

    fn decode(&mut self, buf: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        // the message length header must be present at the minimum, otherwise
        // we can't determine the message type
        if buf.remaining() < 4 {
            return Ok(None);
        }

        // `get_*` integer extractors consume the message bytes by advancing
        // buf's internal cursor. However, we don't want to do this as at this
        // point we aren't sure we have the full message in the buffer, and thus
        // we just want to peek at this value.
        let mut tmp_buf = Cursor::new(&buf);
        let msg_len = tmp_buf.get_u32() as usize;

        tmp_buf.set_position(0);

        if buf.remaining() >= 4 + msg_len {
            // we have the full message in the buffer so advance the buffer
            // cursor past the message length header
            buf.advance(4);
            // the message length is only 0 if this is a keep alive message (all
            // other message types have at least one more field, the message id)
            if msg_len == 0 {
                return Ok(Some(Message::KeepAlive));
            }
        } else {
            log::trace!(
                "Read buffer is {} bytes long but message is {} bytes long",
                buf.remaining(),
                msg_len
            );
            return Ok(None);
        }

        let msg_id = MessageId::try_from(buf.get_u8())?;

        let msg = match msg_id {
            MessageId::Handshake => {
                let pstr_len = buf.get_u8();
                let mut pstr = [0; 19];
                buf.copy_to_slice(&mut pstr);
                let mut reserved = [0; 8];
                buf.copy_to_slice(&mut reserved);
                let mut info_hash = [0; 20];
                buf.copy_to_slice(&mut info_hash);
                let mut peer_id = [0; 20];
                buf.copy_to_slice(&mut peer_id);

                Message::Handshake {
                    pstr_len,
                    pstr,
                    reserved,
                    info_hash,
                    peer_id,
                }
            }
            MessageId::Choke => Message::Choke,
            MessageId::Unchoke => Message::Unchoke,
            MessageId::Interested => Message::Interested,
            MessageId::NotInterested => Message::NotInterested,
            MessageId::Have => {
                let piece_index = buf.get_u32();
                let piece_index = piece_index
                    .try_into()
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
                Message::Have { piece_index }
            }
            MessageId::Bitfield => {
                let mut bitfield = vec![0; msg_len - 1];
                buf.copy_to_slice(&mut bitfield);
                Message::Bitfield(Bitfield::from(bitfield))
            }
            MessageId::Request => {
                let index = buf.get_u32();
                let begin = buf.get_u32();
                let len = buf.get_u32();
                let index = index
                    .try_into()
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;

                Message::Request(BlockInfo { index, begin, len })
            }
            MessageId::Piece => {
                let index = buf.get_u32();
                let index = index
                    .try_into()
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;

                let mut block = vec![0; msg_len - 9];

                buf.copy_to_slice(&mut block);

                Message::Piece(Block {
                    index,
                    begin: buf.get_u32(),
                    block,
                })
            }
            MessageId::Cancel => {
                let index = buf.get_u32();
                let index = index
                    .try_into()
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;

                let begin = buf.get_u32();
                let len = buf.get_u32();

                Message::Cancel(BlockInfo { index, begin, len })
            }
        };

        Ok(Some(msg))
    }
}

/// <pstrlen><pstr><reserved><info_hash><peer_id>
/// <u8_1: pstrlen=19>
/// <u8_19: pstr="BitTorrent protocol">
/// <u8_8: reserved>
/// <u8_20: info_hash>
/// <u8_20: peer_id>
/// This is the very first message exchanged. If the peer's protocol string (`BitTorrent
/// protocol`) or the info hash differs from ours, the connection is severed. The
/// reserved field is 8 zero bytes, but will later be used to set which extensions
/// the peer supports. The peer id is usually the client name and version.
#[derive(Clone, Debug, Writable, Readable)]
pub struct HandshakeOld {
    pub pstr_len: u8,
    pub pstr: [u8; 19],
    pub reserved: [u8; 8],
    pub info_hash: [u8; 20],
    pub peer_id: [u8; 20],
}

impl HandshakeOld {
    pub fn new(info_hash: [u8; 20], peer_id: [u8; 20]) -> Self {
        Self {
            pstr_len: u8::to_be(19),
            pstr: b"BitTorrent protocol".to_owned(),
            reserved: [0u8; 8],
            info_hash,
            peer_id,
        }
    }
    pub fn serialize(&self) -> Result<[u8; 68], Error> {
        let mut buf: [u8; 68] = [0u8; 68];
        let temp = self
            .write_to_vec_with_ctx(BigEndian {})
            .map_err(Error::SpeedyError)?;

        buf.copy_from_slice(&temp[..]);

        Ok(buf)
    }
    pub fn deserialize(buf: &[u8]) -> Result<Self, Error> {
        Self::read_from_buffer_with_ctx(BigEndian {}, buf).map_err(Error::SpeedyError)
    }
    pub fn validate(&self, target: &Self) -> bool {
        if target.peer_id.len() != 20 {
            warn!("! invalid peer_id from receiving handshake");
            return false;
        }
        if self.info_hash != self.info_hash {
            warn!("! info_hash from receiving handshake does not match ours");
            return false;
        }
        if target.pstr_len != 19 {
            warn!("! handshake with wrong pstr_len, dropping connection");
            return false;
        }
        if target.pstr != *b"BitTorrent protocol" {
            warn!("! handshake with wrong pstr, dropping connection");
            return false;
        }
        true
    }
}

// Client connections start out as "choked" and "not interested".
// In other words:
//
// am_choking = 1
// am_interested = 0
// peer_choking = 1
// peer_interested = 0
//
// A block is downloaded by the client,
// when the client is interested in a peer,
// and that peer is not choking the client.
//
// A block is uploaded by a client,
// when the client is not choking a peer,
// and that peer is interested in the client.
//
// c <-handshake-> p
// c <-(optional) bitfield- p
// c -(optional) bitfield-> p
// c -interested-> p
// c <-unchoke- p
// c <-have- p
// c -request-> p
// ~ download starts here ~
// ~ piece contains a block of data ~
// c <-piece- p

//  KeepAlive
//
//  All of the remaining messages in the protocol
//  take the form of: (except for a few)
//  <length prefix><message ID><payload>.
//  The length prefix is a four byte big-endian value.
//  The message ID is a single decimal byte.
//  The payload is message dependent.
//
// <len=0000>

// Choke
// <len=0001><id=0>
// Choke the peer, letting them know that the may _not_ download any pieces.

// Unchoke
// <len=0001><id=1>
// Unchoke the peer, letting them know that the may download.

// Interested
// <len=0001><id=2>
// Let the peer know that we are interested in the pieces that it has
// available.

// Not Interested
// <len=0001><id=3>
// Let the peer know that we are _not_ interested in the pieces that it has
// available because we also have those pieces.

// Have
// <len=0005><id=4><piece index>
// This messages is sent when the peer wishes to announce that they downloaded a
// new piece. This is only sent if the piece's hash verification checked out.

// Bitfield
// <len=0001+X><id=5><bitfield>
// Only ever sent as the first message after the handshake. The payload of this
// message is a bitfield whose indices represent the file pieces in the torrent and
// is used to tell the other peer which pieces the sender has available (each
// available piece's bitfield value is 1). Byte 0 corresponds to indices 0-7, from
// most significant bit to least significant bit, respectively, byte 1 corresponds
// to indices 8-15, and so on. E.g. given the first byte `0b1100'0001` in the
// bitfield means we have pieces 0, 1, and 7.
//
// If a peer doesn't have any pieces downloaded, they need not send
// this message.

// Request
// <len=0013><id=6><index><begin><length>
// This message is sent when a downloader requests a chunk of a file piece from its
// peer. It specifies the piece index, the offset into that piece, and the length
// of the block. As noted above, due to nearly all clients in the wild reject
// requests that are not 16 KiB, we can assume the length field to always be 16
// KiB.
//
// Whether we should also reject requests for different values is an
// open-ended question, as only allowing 16 KiB blocks allows for certain
// optimizations.
// <len=0009+X><id=7><index><begin><block>
// `piece` messages are the responses to `request` messages, containing the request
// block's payload. It is possible for an unexpected piece to arrive if choke and
// unchoke messages are sent in quick succession, if transfer is going slowly, or
// both.

// Cancel
// <len=0013><id=8><index><begin><length>
// Used to cancel an outstanding download request. Generally used towards the end
// of a download in `endgame mode`.
//
// When a download is almost complete, there's a tendency for the last few pieces
// to all be downloaded off a single hosed modem line, taking a very long time. To
// make sure the last few pieces come in quickly, once requests for all pieces a
// given downloader doesn't have yet are currently pending, it sends requests for
// everything to everyone it's downloading from. To keep this from becoming
// horribly inefficient, it sends cancels to everyone else every time a piece
// arrives.
