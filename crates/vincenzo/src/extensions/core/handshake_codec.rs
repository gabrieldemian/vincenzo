//! Codec for encoding and decoding handshakes.
//!
//! This has to be a separate codec as the handshake has a different
//! structure than the rest of the messages. Moreover, handshakes may only
//! be sent once at the beginning of a connection, preceding all other
//! messages. Thus, after receiving and sending a handshake the codec
//! should be switched to [`PeerCodec`], but care should be taken not to
//! discard the underlying receive and send buffers.

use std::{io, io::Cursor};
use tracing::warn;

use bytes::{BufMut, BytesMut};
use speedy::{BigEndian, Readable, Writable};
use tokio_util::codec::{Decoder, Encoder};

use crate::{extensions::core::PSTR, peer::PeerId, torrent::InfoHash};

use bytes::Buf;

use crate::error::Error;

#[derive(Debug)]
pub struct HandshakeCodec;

impl Encoder<Handshake> for HandshakeCodec {
    type Error = io::Error;

    fn encode(
        &mut self,
        handshake: Handshake,
        buf: &mut BytesMut,
    ) -> io::Result<()> {
        let Handshake { pstr_len, pstr, reserved, info_hash, peer_id } =
            handshake;

        // protocol length prefix
        debug_assert_eq!(pstr_len, 19);

        buf.put_u8(pstr.len() as u8);

        // we should only be sending the bittorrent protocol string
        debug_assert_eq!(pstr, PSTR);

        // payload
        buf.extend_from_slice(&pstr);
        buf.extend_from_slice(&reserved);
        buf.extend_from_slice(&info_hash.0);
        buf.extend_from_slice(&peer_id.0);

        Ok(())
    }
}

impl Decoder for HandshakeCodec {
    type Item = Handshake;
    type Error = io::Error;

    fn decode(&mut self, buf: &mut BytesMut) -> io::Result<Option<Handshake>> {
        if buf.is_empty() {
            return Ok(None);
        }

        // `get_*` integer extractors consume the message bytes by advancing
        // buf's internal cursor. However, we don't want to do this as at this
        // point we aren't sure we have the full message in the buffer, and thus
        // we just want to peek at this value.
        let mut tmp_buf = Cursor::new(&buf);
        let prot_len = tmp_buf.get_u8() as usize;
        if prot_len != PSTR.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Handshake must have the string \"BitTorrent protocol\"",
            ));
        }

        // check that we got the full payload in the buffer (NOTE: we need to
        // add the message length prefix's byte count to msg_len since the
        // buffer cursor was not advanced and thus we need to consider the
        // prefix too)
        let payload_len = prot_len + 8 + 20 + 20;
        if buf.remaining() > payload_len {
            // we have the full message in the buffer so advance the buffer
            // cursor past the message length header
            buf.advance(1);
        } else {
            return Ok(None);
        }

        // protocol string
        let mut pstr = [0; 19];
        buf.copy_to_slice(&mut pstr);
        // reserved field
        let mut reserved = [0; 8];
        buf.copy_to_slice(&mut reserved);
        // info hash
        let mut info_hash = [0; 20];
        buf.copy_to_slice(&mut info_hash);
        // peer id
        let mut peer_id = [0; 20];
        buf.copy_to_slice(&mut peer_id);

        Ok(Some(Handshake {
            pstr,
            pstr_len: pstr.len() as u8,
            reserved,
            info_hash: InfoHash(info_hash),
            peer_id: PeerId(peer_id),
        }))
    }
}

/// pstrlen = 19
/// pstr = "BitTorrent protocol"
/// This is the very first message exchanged. If the peer's protocol string
/// (`BitTorrent protocol`) or the info hash differs from ours, the connection
/// is severed. The reserved field is 8 zero bytes, but will later be used to
/// set which extensions the peer supports. The peer id is usually the client
/// name and version.
#[derive(Clone, Debug, Writable, Readable)]
pub struct Handshake {
    pub pstr_len: u8,
    pub pstr: [u8; 19],
    pub reserved: [u8; 8],
    // pub info_hash: [u8; 20],
    pub info_hash: InfoHash,
    // pub peer_id: [u8; 20],
    pub peer_id: PeerId,
}

impl Handshake {
    pub fn new(
        info_hash: impl Into<[u8; 20]>,
        peer_id: impl Into<[u8; 20]>,
    ) -> Self {
        let mut reserved = [0u8; 8];

        // we support the `extension protocol`
        // set the bit 44 to the left
        reserved[5] |= 0x10;

        Self {
            pstr_len: u8::to_be(19),
            pstr: PSTR,
            reserved,
            info_hash: InfoHash(info_hash.into()),
            peer_id: PeerId(peer_id.into()),
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
        Self::read_from_buffer_with_ctx(BigEndian {}, buf)
            .map_err(Error::SpeedyError)
    }
    pub fn validate(&self, target: &Self) -> bool {
        if target.peer_id.0.len() != 20 {
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
pub mod tests {
    use super::*;

    #[test]
    fn handshake() {
        let info_hash = [5u8; 20];
        let peer_id = [7u8; 20];
        let our_handshake = Handshake::new(info_hash, peer_id);

        assert_eq!(our_handshake.pstr_len, 19);
        assert_eq!(our_handshake.pstr, PSTR);
        assert_eq!(our_handshake.peer_id.0, peer_id);
        assert_eq!(our_handshake.info_hash.0, info_hash);

        let our_handshake =
            Handshake::new(info_hash, peer_id).serialize().unwrap();
        assert_eq!(
            our_handshake,
            [
                19, 66, 105, 116, 84, 111, 114, 114, 101, 110, 116, 32, 112,
                114, 111, 116, 111, 99, 111, 108, 0, 0, 0, 0, 0, 16, 0, 0, 5,
                5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 7, 7,
                7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7
            ]
        );
    }
}
