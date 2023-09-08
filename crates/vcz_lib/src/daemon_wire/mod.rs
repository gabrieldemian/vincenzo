///
/// Messages that will be sent betwheen UI <-> Daemon
///
use bytes::{Buf, BufMut, BytesMut};
use speedy::{Readable, Writable};
use std::io::Cursor;
use tokio::io;
use tokio_util::codec::{Decoder, Encoder};

use crate::TorrentState;

/// Messages of `DaemonCodec`.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    /// Quit can be sent to Daemon in 2 ways:
    /// - From TCP (when the UI is quitting)
    /// - From mpsc message (when Daemon is quitting, i.e: through the CLI)
    Quit,
    /// Message sent by clients to add a new Torrent on Daemon
    /// can be sent using the daemon CLI or any UI.
    ///
    /// <len=1+magnet_link_len><id=0><magnet_link>
    NewTorrent(String),
    /// Every second, the Daemon will send information about all torrents
    /// to all listeners
    ///
    /// <len=1+torrent_state_len><id=0><torrent_state>
    TorrentState(TorrentState),
}

#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum MessageId {
    Quit = 0,
    NewTorrent = 1,
    TorrentState = 2,
}

impl TryFrom<u8> for MessageId {
    type Error = io::Error;

    fn try_from(k: u8) -> Result<Self, Self::Error> {
        use MessageId::*;
        match k {
            k if k == Quit as u8 => Ok(Quit),
            k if k == NewTorrent as u8 => Ok(NewTorrent),
            k if k == TorrentState as u8 => Ok(TorrentState),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Unknown message id",
            )),
        }
    }
}

/// The daemon messages follow the same logic as the peer messages:
/// The first `u32` is the len of the entire payload that comes after itself.
/// Followed by an `u8` which is the message_id. The rest of the bytes
/// depends on the message type.
///
/// # Example
///
/// You are sending a magnet of 18 bytes: "magnet:blabla"
///
/// ```
/// let mut buf = BytesMut::new();
/// let magnet = "magnet:blabla".to_owned();
/// // 1 byte reserved for the message_id
/// let msg_len = 1 + magnet.len() as u32;
/// buf.put_u32(msg_len);
/// // this message_id is 0
/// buf.put_u8(MessageId::NewTorrent as u8);
/// buf.extend_from_slice(magnet.as_bytes());
/// ```
#[derive(Debug)]
pub struct DaemonCodec;

// From message to bytes
impl Encoder<Message> for DaemonCodec {
    type Error = io::Error;

    fn encode(&mut self, item: Message, buf: &mut BytesMut) -> Result<(), Self::Error> {
        match item {
            Message::NewTorrent(magnet) => {
                let msg_len = 1 + magnet.len() as u32;

                buf.put_u32(msg_len);
                buf.put_u8(MessageId::NewTorrent as u8);
                buf.extend_from_slice(magnet.as_bytes());
            }
            Message::TorrentState(torrent_info) => {
                let info_bytes = &torrent_info.write_to_vec()?;
                let msg_len = 1 + info_bytes.len() as u32;

                buf.put_u32(msg_len);
                buf.put_u8(MessageId::TorrentState as u8);
                buf.extend_from_slice(info_bytes);
            }
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
            MessageId::Quit => Message::Quit,
            MessageId::NewTorrent => {
                let mut payload = vec![0u8; buf.remaining()];
                buf.copy_to_slice(&mut payload);

                Message::NewTorrent(String::from_utf8(payload).unwrap())
            }
            MessageId::TorrentState => {
                let mut payload = vec![0u8; buf.remaining()];
                buf.copy_to_slice(&mut payload);
                let info = TorrentState::read_from_buffer(&payload)?;

                Message::TorrentState(info)
            }
        };

        Ok(Some(msg))
    }
}

#[cfg(test)]
mod tests {
    use crate::torrent::TorrentStatus;

    use super::*;

    #[test]
    fn new_torrent() {
        let mut buf = BytesMut::new();
        let msg = Message::NewTorrent("magnet:blabla".to_owned());
        DaemonCodec.encode(msg, &mut buf).unwrap();

        println!("encoded {buf:?}");

        let msg = DaemonCodec.decode(&mut buf).unwrap().unwrap();

        println!("decoded {msg:?}");

        match msg {
            Message::NewTorrent(magnet) => {
                assert_eq!(magnet, "magnet:blabla".to_owned());
            }
            _ => panic!(),
        }
    }

    #[test]
    fn torrent_state() {
        let mut buf = BytesMut::new();
        let info = TorrentState {
            name: "Eesti".to_owned(),
            stats: crate::torrent::Stats {
                interval: 5,
                leechers: 9,
                seeders: 1,
            },
            status: TorrentStatus::Downloading,
            downloaded: 999,
            download_rate: 111,
            uploaded: 44,
            size: 9,
            info_hash: [0u8; 20],
        };

        let msg = Message::TorrentState(info.clone());
        DaemonCodec.encode(msg, &mut buf).unwrap();

        println!("encoded {buf:?}");

        let msg = DaemonCodec.decode(&mut buf).unwrap().unwrap();

        println!("decoded {msg:?}");

        match msg {
            Message::TorrentState(deserialized) => {
                assert_eq!(deserialized, info);
            }
            _ => panic!(),
        }
    }
}
