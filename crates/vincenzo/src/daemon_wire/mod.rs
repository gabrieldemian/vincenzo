//! Framed messages sent to/from Daemon
use bytes::{Buf, BufMut, BytesMut};
use int_enum::IntEnum;
use tokio_util::codec::{Decoder, Encoder};
use tracing::warn;

use crate::{
    config::CONFIG_BINCODE,
    error::Error,
    torrent::{InfoHash, TorrentState},
};

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

    /// Daemon will send other Quit messages to all Torrents.
    /// and Disk. It will close all event loops spawned through `run`.
    ///
    /// Quit does not have a message_id, only the u32 len.
    FrontendQuit,

    /// Add a new torrent given a magnet link.
    ///
    /// <len=1+magnet_link_len><id=1><magnet_link>
    NewTorrent(magnet_url::Magnet),

    /// Every second, the Daemon will send information about all torrents
    /// to all listeners
    ///
    /// <len=1+torrent_state_len><id=2><torrent_state>
    TorrentState(TorrentState),

    TorrentStates(Vec<TorrentState>),

    /// Pause/Resume the torrent with the given info_hash.
    TogglePause(InfoHash),

    /// Delete a torrent but doesn't delete any files from disk.
    DeleteTorrent(InfoHash),

    /// Ask the Daemon to send a [`TorrentState`] of the torrent with the given
    GetTorrentState(InfoHash),

    /// Print the status of all Torrents to stdout
    PrintTorrentStatus,
}

#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq, IntEnum)]
pub enum MessageId {
    NewTorrent = 1,
    TorrentState = 2,
    GetTorrentState = 3,
    TogglePause = 4,
    PrintTorrentStatus = 5,
    Quit = 6,
    FrontendQuit = 7,
    TorrentStates = 8,
    DeleteTorrent = 9,
}

#[derive(Debug)]
pub struct DaemonCodec;

// From message to bytes
impl Encoder<Message> for DaemonCodec {
    type Error = Error;

    fn encode(
        &mut self,
        item: Message,
        buf: &mut BytesMut,
    ) -> Result<(), Self::Error> {
        match item {
            Message::NewTorrent(magnet) => {
                let magnet = magnet.to_string();
                let msg_len = 1 + magnet.len() as u32;

                buf.put_u32(msg_len);
                buf.put_u8(MessageId::NewTorrent as u8);
                buf.extend_from_slice(magnet.as_bytes());
            }
            Message::TorrentState(torrent_state) => {
                let bytes =
                    bincode::encode_to_vec(torrent_state, *CONFIG_BINCODE)?;

                let msg_len = 1 + bytes.len() as u32;

                buf.put_u32(msg_len);
                buf.put_u8(MessageId::TorrentState as u8);
                buf.extend_from_slice(&bytes);
            }
            Message::TorrentStates(torrent_states) => {
                let bytes =
                    bincode::encode_to_vec(torrent_states, *CONFIG_BINCODE)?;

                let msg_len = 1 + bytes.len() as u32;

                buf.put_u32(msg_len);
                buf.put_u8(MessageId::TorrentStates as u8);
                buf.extend_from_slice(&bytes);
            }
            Message::GetTorrentState(info_hash) => {
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
            Message::DeleteTorrent(info_hash) => {
                let msg_len = 1 + info_hash.len() as u32;

                buf.put_u32(msg_len);
                buf.put_u8(MessageId::DeleteTorrent as u8);
                buf.extend_from_slice(&*info_hash);
            }
            Message::PrintTorrentStatus => {
                let msg_len = 1;

                buf.put_u32(msg_len);
                buf.put_u8(MessageId::PrintTorrentStatus as u8);
            }
            Message::FrontendQuit => {
                buf.put_u32(1);
                buf.put_u8(MessageId::FrontendQuit as u8);
            }
            Message::Quit => {
                buf.put_u32(1);
                buf.put_u8(MessageId::Quit as u8);
            }
        }
        Ok(())
    }
}

// From bytes to message
impl Decoder for DaemonCodec {
    type Item = Message;
    type Error = Error;

    fn decode(
        &mut self,
        buf: &mut BytesMut,
    ) -> Result<Option<Self::Item>, Self::Error> {
        // the message length header must be present at the minimum, otherwise
        // we can't determine the message type
        if buf.remaining() < 4 {
            return Ok(None);
        }

        // cursor is at <size_u32>
        // peek at length prefix without consuming
        let size =
            u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;

        if size == 0 {
            return Ok(Some(Message::Quit));
        }

        // incomplete message, wait for more
        if buf.len() < 4 + size {
            if buf.capacity() < size {
                buf.reserve((size + 4) - buf.capacity());
            }
            return Ok(None);
        }

        // advance past the size, into the msg_id
        buf.advance(4);

        let msg_id = buf.get_u8();

        let Ok(msg_id) = MessageId::try_from(msg_id) else {
            warn!("unknown message_id {msg_id:?}");
            buf.advance(size - 1);
            return Ok(None);
        };

        let msg = match msg_id {
            MessageId::NewTorrent => {
                let mut payload = vec![0u8; buf.remaining()];
                buf.copy_to_slice(&mut payload);

                let magnet =
                    magnet_url::Magnet::new(&String::from_utf8(payload)?)?;

                Message::NewTorrent(magnet)
            }
            MessageId::TorrentState => {
                let mut payload = vec![0u8; buf.remaining()];
                buf.copy_to_slice(&mut payload);

                let info: (TorrentState, usize) =
                    bincode::decode_from_slice(&payload, *CONFIG_BINCODE)?;

                Message::TorrentState(info.0)
            }
            MessageId::TorrentStates => {
                let mut payload = vec![0u8; buf.remaining()];
                buf.copy_to_slice(&mut payload);

                let states: Vec<TorrentState> =
                    bincode::decode_from_slice(&payload, *CONFIG_BINCODE)
                        .map(|v| v.0)?;

                Message::TorrentStates(states)
            }
            MessageId::TogglePause => {
                let mut payload = [0u8; 20_usize];
                buf.copy_to_slice(&mut payload);

                Message::TogglePause(payload.into())
            }
            MessageId::DeleteTorrent => {
                let mut payload = [0u8; 20_usize];
                buf.copy_to_slice(&mut payload);

                Message::DeleteTorrent(payload.into())
            }
            MessageId::PrintTorrentStatus => Message::PrintTorrentStatus,
            MessageId::Quit => Message::Quit,
            MessageId::FrontendQuit => Message::FrontendQuit,
            MessageId::GetTorrentState => {
                let mut payload = [0u8; 20_usize];
                buf.copy_to_slice(&mut payload);

                Message::GetTorrentState(payload.into())
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
    fn torrent_states() {
        let infos = vec![
            TorrentState {
                name: "Eesti".to_owned(),
                stats: crate::torrent::Stats {
                    interval: 5,
                    leechers: 9,
                    seeders: 1,
                },
                ..Default::default()
            },
            TorrentState {
                name: "France".to_owned(),
                stats: crate::torrent::Stats {
                    interval: 2,
                    leechers: 8,
                    seeders: 10,
                },
                ..Default::default()
            },
        ];

        let mut buf = BytesMut::new();
        let msg = Message::TorrentStates(infos.clone());
        DaemonCodec.encode(msg, &mut buf).unwrap();

        let msg = DaemonCodec.decode(&mut buf).unwrap().unwrap();

        match msg {
            Message::TorrentStates(deserialized) => {
                assert_eq!(deserialized, infos);
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
            download_rate: 111.0,
            uploaded: 44,
            size: 9,
            info_hash: [0u8; 20].into(),
            ..Default::default()
        };

        let a = bincode::encode_to_vec(&info, *CONFIG_BINCODE).unwrap();
        println!("encoding a {a:?}");

        let mut buf = BytesMut::new();
        let msg = Message::TorrentState(info.clone());
        DaemonCodec.encode(msg, &mut buf).unwrap();

        let msg = DaemonCodec.decode(&mut buf).unwrap().unwrap();

        match msg {
            Message::TorrentState(deserialized) => {
                assert_eq!(deserialized, info);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn request_torrent_state() {
        let mut buf = BytesMut::new();
        let msg = Message::GetTorrentState([1u8; 20].into());
        DaemonCodec.encode(msg, &mut buf).unwrap();

        println!("encoded {buf:?}");

        let msg = DaemonCodec.decode(&mut buf).unwrap().unwrap();

        println!("decoded {msg:?}");

        match msg {
            Message::GetTorrentState(info_hash) => {
                assert_eq!(info_hash.0, [1u8; 20]);
            }
            _ => panic!(),
        }
    }
}
