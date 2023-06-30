use bytes::{Buf, BufMut, BytesMut};
use log::{debug, warn};
use speedy::{BigEndian, Readable, Writable};
use std::io::Cursor;
use tokio::io;
use tokio_util::codec::{Decoder, Encoder};

use crate::{bitfield::Bitfield, error::Error, tcp_wire::lib::Block};

use super::lib::{BlockInfo, PSTR};

// Handshake is an edge-case message,
// it will be sent separately from the codec,
// before any other message
#[derive(Debug, Clone)]
pub enum Message {
    KeepAlive,
    Bitfield(Bitfield),
    Choke,
    Unchoke,
    Interested,
    NotInterested,
    Have(usize),
    Request(BlockInfo),
    Piece(Block),
    Cancel(BlockInfo),
    Extended((u8, Vec<u8>)),
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
    Extended = 20,
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
            k if k == Extended as u8 => Ok(Extended),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Unknown message id",
            )),
        }
    }
}

#[derive(Debug)]
pub struct PeerCodec;

// Encode bytes into messages
impl Encoder<Message> for PeerCodec {
    type Error = io::Error;

    fn encode(&mut self, item: Message, buf: &mut BytesMut) -> Result<(), Self::Error> {
        match item {
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
            Message::Have(piece_index) => {
                let msg_len = 1 + 4;
                buf.put_u32(msg_len);
                buf.put_u8(MessageId::Have as u8);
                let piece_index = piece_index
                    .try_into()
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
                buf.put_u32(piece_index);
            }
            // <len=0013><id=6><index><begin><length>
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
            Message::Extended((ext_id, payload)) => {
                let msg_len = payload.len() as u32 + 2;
                buf.put_u32(msg_len);
                buf.put_u8(MessageId::Extended as u8);
                buf.put_u8(ext_id);

                if payload.len() > 0 {
                    buf.extend_from_slice(&payload);
                }
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
            // <len=0001><id=0>
            MessageId::Choke => Message::Choke,
            // <len=0001><id=1>
            MessageId::Unchoke => Message::Unchoke,
            // <len=0001><id=2>
            MessageId::Interested => Message::Interested,
            // <len=0001><id=3>
            MessageId::NotInterested => Message::NotInterested,
            // <len=0005><id=4><piece index>
            MessageId::Have => {
                let piece_index = buf.get_u32();
                Message::Have(piece_index as usize)
            }
            // <len=0001+X><id=5><bitfield>
            MessageId::Bitfield => {
                let mut bitfield = vec![0; msg_len - 1];
                buf.copy_to_slice(&mut bitfield);
                Message::Bitfield(Bitfield::from(bitfield))
            }
            // <len=0013><id=6><index><begin><length>
            MessageId::Request => {
                let index = buf.get_u32();
                let begin = buf.get_u32();
                let len = buf.get_u32();

                Message::Request(BlockInfo { index, begin, len })
            }
            // <len=0009+X><id=7><index><begin><block>
            MessageId::Piece => {
                let index = buf.get_u32() as usize;
                let begin = buf.get_u32();

                let mut block = vec![0; msg_len - 9];
                buf.copy_to_slice(&mut block);

                Message::Piece(Block {
                    index,
                    begin,
                    block,
                })
            }
            // <len=0013><id=8><index><begin><length>
            MessageId::Cancel => {
                let index = buf.get_u32();
                let begin = buf.get_u32();
                let len = buf.get_u32();

                Message::Cancel(BlockInfo { index, begin, len })
            }
            MessageId::Extended => {
                let ext_id = buf.get_u8();

                let mut payload = vec![0u8; msg_len - 2];
                buf.copy_to_slice(&mut payload);

                // debug!("payload is {payload:?}");
                // debug!("decoding payload {:?}", String::from_utf8_lossy(&payload));

                // d8:msg_typei1e5:piecei0e10:total_sizei5205eed5:filesld6:lengthi4092334e4:pathl62:Kerkour S. Black Hat Rust...Rust programming language 2022.pdfeee4:name58:Kerkour S. Black Hat Rust...Rust programming language 202212:piece lengthi16384e6:pieces5000:7\u{1a}ǚ\

                Message::Extended((ext_id, payload))
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
pub struct Handshake {
    pub pstr_len: u8,
    pub pstr: [u8; 19],
    pub reserved: [u8; 8],
    pub info_hash: [u8; 20],
    pub peer_id: [u8; 20],
}

impl Handshake {
    pub fn new(info_hash: [u8; 20], peer_id: [u8; 20]) -> Self {
        let mut reserved = [0u8; 8];

        // we support the `extension protocol`
        reserved[5] |= 0x10;

        Self {
            pstr_len: u8::to_be(19),
            pstr: PSTR,
            reserved,
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
        if self.info_hash != target.info_hash {
            warn!("! info_hash from receiving handshake does not match ours");
            return false;
        }
        if target.pstr_len != 19 {
            warn!("! handshake with wrong pstr_len, dropping connection");
            return false;
        }
        if target.pstr != PSTR {
            warn!("! handshake with wrong pstr, dropping connection");
            return false;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use crate::tcp_wire::lib::BLOCK_LEN;

    use super::*;
    use bitlab::SingleBits;
    use bytes::{Buf, BytesMut};
    use tokio_util::codec::{Decoder, Encoder};

    #[test]
    fn extended() {
        let mut buf = BytesMut::new();
        let msg = Message::Extended((0, vec![]));
        PeerCodec.encode(msg.clone(), &mut buf).unwrap();

        // len
        assert_eq!(buf.len(), 6);
        // len prefix
        assert_eq!(buf.get_u32(), 2);
        // ext id
        assert_eq!(buf.get_u8(), MessageId::Extended as u8);
        // ext id
        assert_eq!(buf.get_u8(), 0);

        let mut buf = BytesMut::new();
        PeerCodec.encode(msg, &mut buf).unwrap();
        let msg = PeerCodec.decode(&mut buf).unwrap().unwrap();

        match msg {
            Message::Extended((ext_id, _payload)) => {
                assert_eq!(ext_id, 0);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn request() {
        let mut buf = BytesMut::new();
        let msg = Message::Request(BlockInfo::default());
        PeerCodec.encode(msg.clone(), &mut buf).unwrap();

        // size of buf
        assert_eq!(buf.len(), 17);
        // len
        assert_eq!(buf.get_u32(), 13);
        // id
        assert_eq!(buf.get_u8(), MessageId::Request as u8);
        // index
        assert_eq!(buf.get_u32(), 0);
        // begin
        assert_eq!(buf.get_u32(), 0);
        // len of block
        assert_eq!(buf.get_u32(), BLOCK_LEN);

        let mut buf = BytesMut::new();
        PeerCodec.encode(msg, &mut buf).unwrap();
        let msg = PeerCodec.decode(&mut buf).unwrap().unwrap();

        match msg {
            Message::Request(block_info) => {
                assert_eq!(block_info.index, 0);
                assert_eq!(block_info.begin, 0);
                assert_eq!(block_info.len, BLOCK_LEN);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn piece() {
        let mut buf = BytesMut::new();
        let msg = Message::Piece(Block {
            index: 0,
            begin: 0,
            block: vec![0],
        });
        PeerCodec.encode(msg.clone(), &mut buf).unwrap();

        // len
        assert_eq!(buf.get_u32(), 9 + 1);
        // id
        assert_eq!(buf.get_u8(), MessageId::Piece as u8);
        // index
        assert_eq!(buf.get_u32(), 0);
        // begin
        assert_eq!(buf.get_u32(), 0);
        // block
        let mut block = BytesMut::new();
        buf.copy_to_slice(&mut block);
        assert_eq!(block.len(), 0);

        let mut buf = BytesMut::new();
        PeerCodec.encode(msg.clone(), &mut buf).unwrap();
        let msg = PeerCodec.decode(&mut buf).unwrap().unwrap();

        match msg {
            Message::Piece(block) => {
                assert_eq!(block.index, 0);
                assert_eq!(block.begin, 0);
                assert_eq!(block.block.len(), 1);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn handshake() {
        let info_hash = [5u8; 20];
        let peer_id = [7u8; 20];
        let our_handshake = Handshake::new(info_hash, peer_id);

        assert_eq!(our_handshake.pstr_len, 19);
        assert_eq!(our_handshake.pstr, PSTR);
        assert_eq!(our_handshake.peer_id, peer_id);
        assert_eq!(our_handshake.info_hash, info_hash);

        let our_handshake = Handshake::new(info_hash, peer_id).serialize().unwrap();
        assert_eq!(
            our_handshake,
            [
                19, 66, 105, 116, 84, 111, 114, 114, 101, 110, 116, 32, 112, 114, 111, 116, 111,
                99, 111, 108, 0, 0, 0, 0, 0, 16, 0, 0, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5,
                5, 5, 5, 5, 5, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7
            ]
        );
    }

    #[test]
    fn reserved_bytes() {
        let mut reserved = [0u8; 8];
        reserved[5] |= 0x10;

        assert_eq!(reserved, [0, 0, 0, 0, 0, 16, 0, 0]);

        let support_extension_protocol = reserved[5].get_bit(3).unwrap();

        assert!(support_extension_protocol)
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
// c <-extended (if supported)->p
// c <-(optional) bitfield-> p
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
