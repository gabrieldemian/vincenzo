//! Framed messages sent to/from Daemon
use bytes::{Buf, BufMut, BytesMut};
use speedy::{BigEndian, Readable, Writable};
use std::io::Cursor;
use tokio::io;
use tokio_util::codec::{Decoder, Encoder};

use crate::torrent::{InfoHash, TorrentState};

/// Messages of [`DaemonCodec`], check the struct documentation
/// to read how to send messages.
///
/// Most messages can be sent to the Daemon in 2 ways:
/// - Internally: within it's same process, via CLI flags for example. the
///   message will be sent using mpsc.
/// - Externally: via TCP. When the message arrives, it will be sent to the
///   internal event handler in mpsc. They both use the same API.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    /// Daemon will send other Quit messages to all Torrents.
    /// and Disk. It will close all event loops spawned through `run`.
    ///
    /// Quit does not have a message_id, only the u32 len.
    Quit,
    /// Add a new torrent given a magnet link.
    ///
    /// <len=1+magnet_link_len><id=1><magnet_link>
    NewTorrent(String),
    /// Every second, the Daemon will send information about all torrents
    /// to all listeners
    ///
    /// <len=1+torrent_state_len><id=2><torrent_state>
    TorrentState(TorrentState),
    /// Pause/Resume the torrent with the given info_hash.
    TogglePause(InfoHash),
    /// Ask the Daemon to send a [`TorrentState`] of the torrent with the given
    RequestTorrentState(InfoHash),
    /// Print the status of all Torrents to stdout
    PrintTorrentStatus,
}

#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum MessageId {
    NewTorrent = 1,
    TorrentState = 2,
    GetTorrentState = 3,
    TogglePause = 4,
    PrintTorrentStatus = 5,
}

impl TryFrom<u8> for MessageId {
    type Error = io::Error;

    fn try_from(k: u8) -> Result<Self, Self::Error> {
        use MessageId::*;
        match k {
            k if k == NewTorrent as u8 => Ok(NewTorrent),
            k if k == TorrentState as u8 => Ok(TorrentState),
            k if k == GetTorrentState as u8 => Ok(GetTorrentState),
            k if k == PrintTorrentStatus as u8 => Ok(PrintTorrentStatus),
            k if k == TogglePause as u8 => Ok(TogglePause),
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
/// In other words:
/// len,msg_id,payload
///  u32    u8       x      (in bits)
///
/// # Example
///
/// You are sending a magnet of 18 bytes: "magnet:blabla"
///
/// ```
/// use bytes::{Buf, BufMut, BytesMut};
/// use vincenzo::daemon_wire::MessageId;
///
/// let mut buf = BytesMut::new();
/// let magnet = "magnet:blabla".to_owned();
///
/// // len is: 1 byte of the message_id + the payload len
/// let msg_len = 1 + magnet.len() as u32;
///
/// // len>
/// buf.put_u32(msg_len);
///
/// // msg_id message_id is 1
/// buf.put_u8(MessageId::NewTorrent as u8);
///
/// // payload
/// buf.extend_from_slice(magnet.as_bytes());
///
/// // result
/// // len  msg_id  payload
/// // 19     1    "magnet:blabla"
/// // u32    u8    (dynamic size)
/// ```
#[derive(Debug)]
pub struct DaemonCodec;

// From message to bytes
impl Encoder<Message> for DaemonCodec {
    type Error = io::Error;

    fn encode(
        &mut self,
        item: Message,
        buf: &mut BytesMut,
    ) -> Result<(), Self::Error> {
        match item {
            Message::NewTorrent(magnet) => {
                let msg_len = 1 + magnet.len() as u32;

                buf.put_u32(msg_len);
                buf.put_u8(MessageId::NewTorrent as u8);
                buf.extend_from_slice(magnet.as_bytes());
            }
            Message::TorrentState(torrent_state) => {
                let info_bytes =
                    torrent_state.write_to_vec_with_ctx(BigEndian {})?;

                let msg_len = 1 + info_bytes.len() as u32;

                buf.put_u32(msg_len);
                buf.put_u8(MessageId::TorrentState as u8);
                buf.extend_from_slice(&info_bytes);
            }
            Message::RequestTorrentState(info_hash) => {
                let msg_len = 1 + info_hash.len() as u32;

                buf.put_u32(msg_len);
                buf.put_u8(MessageId::GetTorrentState as u8);
                buf.extend_from_slice(&*info_hash);
            }
            Message::TogglePause(info_hash) => {
                let msg_len = 1 + info_hash.len() as u32;

                buf.put_u32(msg_len);
                buf.put_u8(MessageId::TogglePause as u8);
                buf.extend_from_slice(&*info_hash);
            }
            Message::PrintTorrentStatus => {
                let msg_len = 1;

                buf.put_u32(msg_len);
                buf.put_u8(MessageId::PrintTorrentStatus as u8);
            }
            Message::Quit => {
                buf.put_u32(0);
            }
        }
        Ok(())
    }
}

// From bytes to message
impl Decoder for DaemonCodec {
    type Item = Message;
    type Error = io::Error;

    fn decode(
        &mut self,
        buf: &mut BytesMut,
    ) -> Result<Option<Self::Item>, Self::Error> {
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

            // Only a Quit doesnt have ID
            if msg_len == 0 {
                return Ok(Some(Message::Quit));
            }
        } else {
            tracing::trace!(
                "Read buffer is {} bytes long but message is {} bytes long",
                buf.remaining(),
                msg_len
            );
            return Ok(None);
        }

        let msg_id = MessageId::try_from(buf.get_u8())?;

        // here, buf is already advanced past the len and msg_id,
        // so all calls to `remaining` and `get_*` will start from the payload.
        let msg = match msg_id {
            MessageId::NewTorrent => {
                let mut payload = vec![0u8; buf.remaining()];
                buf.copy_to_slice(&mut payload);

                Message::NewTorrent(String::from_utf8(payload).unwrap())
            }
            MessageId::TorrentState => {
                let mut payload = vec![0u8; buf.remaining()];

                buf.copy_to_slice(&mut payload);

                let info = TorrentState::read_from_buffer_with_ctx(
                    BigEndian {},
                    &payload,
                )?;

                Message::TorrentState(info)
            }
            MessageId::TogglePause => {
                let mut payload = [0u8; 20_usize];
                buf.copy_to_slice(&mut payload);

                Message::TogglePause(payload.into())
            }
            MessageId::PrintTorrentStatus => Message::PrintTorrentStatus,
            MessageId::GetTorrentState => {
                let mut payload = [0u8; 20_usize];
                buf.copy_to_slice(&mut payload);

                Message::RequestTorrentState(payload.into())
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
            info_hash: [0u8; 20].into(),
            ..Default::default()
        };

        let a = info.write_to_vec_with_ctx(BigEndian {}).unwrap();
        println!("encoding a {a:?}");

        let mut buf = BytesMut::new();
        let msg = Message::TorrentState(Some(info.clone()));
        DaemonCodec.encode(msg, &mut buf).unwrap();

        let msg = DaemonCodec.decode(&mut buf).unwrap().unwrap();

        match msg {
            Message::TorrentState(deserialized) => {
                assert_eq!(deserialized, Some(info));
            }
            _ => panic!(),
        }

        // should send None to inexistent torrent
        let mut buf = BytesMut::new();
        let msg = Message::TorrentState(None);
        DaemonCodec.encode(msg, &mut buf).unwrap();

        let msg = DaemonCodec.decode(&mut buf).unwrap().unwrap();
        match msg {
            Message::TorrentState(r) => {
                assert_eq!(r, None);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn request_torrent_state() {
        let mut buf = BytesMut::new();
        let msg = Message::RequestTorrentState([1u8; 20].into());
        DaemonCodec.encode(msg, &mut buf).unwrap();

        println!("encoded {buf:?}");

        let msg = DaemonCodec.decode(&mut buf).unwrap().unwrap();

        println!("decoded {msg:?}");

        match msg {
            Message::RequestTorrentState(info_hash) => {
                assert_eq!(info_hash, [1u8; 20].into());
            }
            _ => panic!(),
        }
    }
}
