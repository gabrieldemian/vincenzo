///
/// Messages that will be sent betwheen UI <-> Daemon
///
use bytes::{Buf, BufMut, BytesMut};
use hashbrown::HashMap;
use speedy::Writable;
// use speedy::{BigEndian, Readable, Writable};
use std::io::Cursor;
use tokio::io;
use tokio_util::codec::{Decoder, Encoder};

use crate::TorrentInfo;

#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    /// Every second, the Daemon will send information about all torrents
    /// to all listeners
    TorrentsState(HashMap<[u8; 20], TorrentInfo>),
    /// Torrents must send this message to Daemon
    /// every second with updated state
    TorrentUpdate(TorrentInfo),
    /// Message sent by clients to add a new Torrent on Daemon
    /// can be sent using the daemon CLI or any UI.
    /// The first argument is the magnet string.
    NewTorrent(String),
}

#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum MessageId {
    NewTorrent = 0,
}

impl TryFrom<u8> for MessageId {
    type Error = io::Error;

    fn try_from(k: u8) -> Result<Self, Self::Error> {
        use MessageId::*;
        match k {
            k if k == NewTorrent as u8 => Ok(NewTorrent),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Unknown message id",
            )),
        }
    }
}

#[derive(Debug)]
pub struct DaemonCodec;

// From message to bytes
impl Encoder<Message> for DaemonCodec {
    type Error = io::Error;

    fn encode(&mut self, item: Message, buf: &mut BytesMut) -> Result<(), Self::Error> {
        match item {
            Message::NewTorrent(magnet) => {
                // 1 byte is for the ID of the mssage.
                let msg_len = 1 + magnet.len() as u32;
                buf.put_u32(msg_len);
                buf.put_u8(MessageId::NewTorrent as u8);
                buf.extend_from_slice(magnet.as_bytes());
            }
            Message::TorrentsState(torrent_infos) => {
                let mut msg_len = 1;
                let mut payload: Vec<u8> = vec![];

                for info in torrent_infos.values() {
                    buf.extend_from_slice(&info.write_to_vec()?);
                    // buf.extend_from_slice(&info.info_hash);
                }
            } // Message::Bitfield(bitfield) => {
            //     buf.put_u32(1 + bitfield.len_bytes() as u32);
            //     buf.put_u8(MessageId::Bitfield as u8);
            //     buf.extend_from_slice(bitfield.inner.as_slice());
            // }
            // <len=0013><id=6><index><begin><length>
            _ => todo!(),
        }
        Ok(())
    }
}

// From bytes to message
impl Decoder for DaemonCodec {
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

        // if tmp_buf.get_u8() == 20 {
        //     println!("received ext msg, showing raw buf {:?}", buf.to_vec());
        // }

        tmp_buf.set_position(0);

        if buf.remaining() >= 4 + msg_len {
            // we have the full message in the buffer so advance the buffer
            // cursor past the message length header
            buf.advance(4);
        } else {
            tracing::trace!(
                "Read buffer is {} bytes long but message is {} bytes long",
                buf.remaining(),
                msg_len
            );
            return Ok(None);
        }

        let msg_id = MessageId::try_from(buf.get_u8())?;

        // note: buf is already advanced past the len,
        // so all calls to `remaining` will get the payload, excluding the len.
        let msg = match msg_id {
            MessageId::NewTorrent => {
                let mut payload = vec![0u8; buf.remaining()];
                buf.copy_to_slice(&mut payload);

                Message::NewTorrent(String::from_utf8(payload).unwrap())
            }
            _ => todo!(),
        };

        Ok(Some(msg))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_torrent() {
        let mut buf = BytesMut::new();
        let msg = Message::NewTorrent("magnet:blabla".to_owned());
        DaemonCodec.encode(msg, &mut buf).unwrap();

        println!("encoded {buf:?}");

        // let mut buf = BytesMut::new();
        let msg = DaemonCodec.decode(&mut buf).unwrap().unwrap();

        println!("decoded {msg:?}");

        match msg {
            Message::NewTorrent(magnet) => {
                assert_eq!(magnet, "magnet:blabla".to_owned());
            }
            _ => panic!(),
        }
    }
}
