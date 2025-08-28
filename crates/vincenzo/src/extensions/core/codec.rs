use bytes::{Buf, BufMut, BytesMut};
use int_enum::IntEnum;
use std::sync::atomic::Ordering;
use tokio::{io, sync::oneshot};
use tokio_util::codec::{Decoder, Encoder};
use tracing::{debug, warn};
use vincenzo_macros::Message;

use super::{Block, BlockInfo};
use crate::{
    bitfield::Bitfield,
    disk::DiskMsg,
    error::Error,
    extensions::{ExtData, ExtMsg, ExtMsgHandler},
    peer::{self, MsgHandler},
    torrent::TorrentMsg,
};

pub static MAX_MESSAGE_SIZE: usize = 2 * 1024 * 1024; // 2MB maximum message size

/// State that comes with the Core protocol.
#[derive(Clone)]
pub struct CoreState {
    /// If we're choked, peer doesn't allow us to download pieces from them.
    pub am_choking: bool,

    /// If we're interested, peer has pieces that we don't have.
    pub am_interested: bool,

    /// If peer is choked, we don't allow them to download pieces from us.
    pub peer_choking: bool,

    /// If peer is interested in us, they mean to download pieces that we have.
    pub peer_interested: bool,
}

impl ExtMsg for Core {
    // not really used since core does not use this.
    const ID: u8 = u8::MAX;
}

impl ExtData for CoreState {}

impl Default for CoreState {
    /// By default, both sides of the connection start off as choked and not
    /// interested in the other.
    fn default() -> Self {
        Self {
            am_choking: true,
            am_interested: false,
            peer_choking: true,
            peer_interested: false,
        }
    }
}

impl Core {
    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> usize {
        use Core::*;
        match self {
            KeepAlive => 4,
            Choke | Unchoke | Interested | NotInterested => 4 + 1,
            Cancel(_) | Request(_) => 4 + 4 + 4,
            Have(_) => 4 + 1 + 4,
            Bitfield(b) => 4 + 1 + b.to_bitvec().len(),
            Piece(b) => 4 + 1 + 4 + 4 + b.block.len(),
            Extended(m) => 4 + 1 + 1 + m.1.len(),
        }
    }
}

/// The first value is decided when the peer sends its extension header, in the
/// m field.
#[derive(Debug, Clone, PartialEq)]
pub struct ExtendedMessage(pub u8, pub Vec<u8>);

impl From<ExtendedMessage> for Vec<u8> {
    fn from(val: ExtendedMessage) -> Self {
        let mut buff = Vec::with_capacity(1 + val.1.len());
        buff.push(val.0);
        buff.extend(val.1);
        buff
    }
}

impl From<ExtendedMessage> for Core {
    fn from(value: ExtendedMessage) -> Self {
        Self::Extended(value)
    }
}

/// Core messages exchanged after a successful handshake.
/// These are from the vanilla protocol, with no extensions.
#[repr(u8)]
#[derive(Debug, Clone, PartialEq, Message)]
pub enum Core {
    Choke = 0,
    Unchoke = 1,
    Interested = 2,
    NotInterested = 3,
    Have(usize) = 4,
    Bitfield(Bitfield) = 5,
    Request(BlockInfo) = 6,
    Piece(Block) = 7,
    Cancel(BlockInfo) = 8,
    Extended(ExtendedMessage) = 20,
    KeepAlive,
}

/// The IDs of the [`Core`] messages.
#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq, IntEnum)]
pub enum CoreId {
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

#[derive(Debug, Clone)]
pub struct CoreCodec;

#[derive(Clone, Debug, Copy)]
pub struct CoreExt;

impl TryInto<Vec<u8>> for Core {
    type Error = Error;

    fn try_into(self) -> Result<Vec<u8>, Error> {
        let mut dst = BytesMut::new();
        let mut codec = CoreCodec;
        codec.encode(self, &mut dst)?;
        Ok(dst.into())
    }
}

impl From<Core> for BytesMut {
    fn from(val: Core) -> Self {
        let mut dst = BytesMut::new();
        let _ = CoreCodec.encode(val, &mut dst);
        dst
    }
}

impl ExtMsgHandler<Core, CoreState> for MsgHandler {
    async fn handle_msg(
        &self,
        peer: &mut peer::Peer<peer::Connected>,
        msg: Core,
    ) -> Result<(), Error> {
        match msg {
            // handled by the extended messages
            Core::Extended(_) => {}
            Core::KeepAlive => {
                debug!("< keepalive");
            }
            Core::Bitfield(bitfield) => {
                debug!(
                    "< bitfield len: {} ones: {}",
                    bitfield.len(),
                    bitfield.count_ones()
                );

                peer.state
                    .ctx
                    .torrent_ctx
                    .tx
                    .send(TorrentMsg::SetPeerBitfield(
                        peer.state.ctx.id.clone(),
                        bitfield.clone(),
                    ))
                    .await?;
            }
            Core::Unchoke => {
                peer.state.ctx.peer_choking.store(false, Ordering::Relaxed);
                peer.state_log[3] = 'u';
                debug!("< unchoke");
            }
            Core::Choke => {
                debug!("< choke");
                peer.state.ctx.peer_choking.store(true, Ordering::Relaxed);
                peer.free_pending_blocks();
                peer.state_log[3] = '-';
            }
            Core::Interested => {
                // remote peer is interested the local peer
                debug!("< interested");
                peer.state.ctx.peer_interested.store(true, Ordering::Relaxed);
                peer.state_log[4] = 'i';
            }
            Core::NotInterested => {
                debug!("< not_interested");
                peer.state.ctx.peer_interested.store(false, Ordering::Relaxed);
                peer.state_log[4] = '-';
            }
            Core::Have(piece) => {
                debug!("< have {piece}");

                peer.state
                    .ctx
                    .torrent_ctx
                    .tx
                    .send(TorrentMsg::PeerHave(
                        peer.state.ctx.id.clone(),
                        piece,
                    ))
                    .await?;
            }
            Core::Piece(block) => {
                peer.handle_block(block).await?;
            }
            Core::Cancel(block_info) => {
                debug!("< cancel {block_info:?}");
                peer.state.incoming_requests.retain(|v| *v != block_info);
            }
            Core::Request(block_info) => {
                debug!("< request {block_info:?}");

                if peer.state.ctx.peer_choking.load(Ordering::Relaxed) {
                    return Ok(());
                }

                let (tx, rx) = oneshot::channel();

                peer.state.incoming_requests.push(block_info.clone());

                peer.state
                    .ctx
                    .torrent_ctx
                    .disk_tx
                    .send(DiskMsg::ReadBlock {
                        block_info,
                        recipient: tx,
                        info_hash: peer.state.ctx.torrent_ctx.info_hash.clone(),
                    })
                    .await?;

                peer.send(Core::Piece(rx.await?)).await?;
            }
        }

        Ok(())
    }
}

impl Encoder<Core> for CoreCodec {
    type Error = Error;

    fn encode(
        &mut self,
        item: Core,
        buf: &mut BytesMut,
    ) -> Result<(), Self::Error> {
        match item {
            Core::KeepAlive => {
                buf.put_u32(0);
            }
            Core::Bitfield(bitfield) => {
                let v = bitfield.into_vec();
                buf.put_u32(1 + v.len() as u32);
                buf.put_u8(CoreId::Bitfield as u8);
                buf.extend_from_slice(&v);
            }
            Core::Choke => {
                buf.put_u32(1);
                buf.put_u8(CoreId::Choke as u8);
            }
            Core::Unchoke => {
                buf.put_u32(1);
                buf.put_u8(CoreId::Unchoke as u8);
            }
            Core::Interested => {
                buf.put_u32(1);
                buf.put_u8(CoreId::Interested as u8);
            }
            Core::NotInterested => {
                buf.put_u32(1);
                buf.put_u8(CoreId::NotInterested as u8);
            }
            Core::Have(piece_index) => {
                let msg_len = 1 + 4;
                buf.put_u32(msg_len);
                buf.put_u8(CoreId::Have as u8);
                let piece_index = piece_index.try_into().map_err(|e| {
                    io::Error::new(io::ErrorKind::InvalidInput, e)
                })?;
                buf.put_u32(piece_index);
            }
            // <len=0013><id=6><index><begin><length>
            Core::Request(block) => {
                let msg_len = 1 + 4 + 4 + 4;
                buf.put_u32(msg_len);
                buf.put_u8(CoreId::Request as u8);
                block.encode(buf)?;
            }
            Core::Piece(block) => {
                let Block { index, begin, block } = block;

                let msg_len = 1 + 4 + 4 + block.len() as u32;

                buf.put_u32(msg_len);
                buf.put_u8(CoreId::Piece as u8);

                let index = index.try_into().map_err(|e| {
                    io::Error::new(io::ErrorKind::InvalidInput, e)
                })?;

                buf.put_u32(index);
                buf.put_u32(begin);
                buf.put(&block[..]);
            }
            Core::Cancel(block) => {
                let msg_len = 1 + 4 + 4 + 4;

                buf.put_u32(msg_len);
                buf.put_u8(CoreId::Cancel as u8);

                block.encode(buf)?;
            }
            Core::Extended(ExtendedMessage(ext_id, payload)) => {
                let msg_len = payload.len() as u32 + 2;

                buf.put_u32(msg_len);
                buf.put_u8(CoreId::Extended as u8);
                buf.put_u8(ext_id);
                buf.extend_from_slice(&payload);
            }
        }
        Ok(())
    }
}

impl Decoder for CoreCodec {
    type Item = Core;
    type Error = Error;

    fn decode(
        &mut self,
        buf: &mut BytesMut,
    ) -> Result<Option<Self::Item>, Self::Error> {
        // the message length header must be present at the minimum, otherwise
        // we can't determine the message type
        if buf.len() < 4 {
            return Ok(None);
        }

        // cursor is at <size_u32>

        // peek at length prefix without consuming
        let size =
            u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;

        if size == 0 {
            buf.advance(4);
            return Ok(Some(Core::KeepAlive));
        }

        // incomplete message, if the packet is to large to fit the MTU (~1,500
        // bytes) the packet will be split into many packets. The decoder will
        // be called each time a packet arrive, but if the buffer is not
        // full yet, we don't avance the cursor and just wait.
        if buf.len() < 4 + size {
            if buf.capacity() < size {
                buf.reserve((size + 4) - buf.capacity());
            }
            return Ok(None);
        }

        // advance past the size, into the msg_id
        buf.advance(4);

        // with keepalive out of the way,
        // all the messages have at least 1 byte of size
        if buf.is_empty() {
            return Ok(None);
        }

        let msg_id = buf.get_u8();

        let Ok(msg_id) = CoreId::try_from(msg_id) else {
            // unknown message id, just skip the segment
            // cursor here is after msg_id
            warn!("unknown message_id {msg_id:?}");
            buf.advance(size - 1);
            // buf.clear();
            return Ok(None);
        };

        // cursor is already past the size here and msg_id,
        // it's into the payload.

        let msg = match msg_id {
            // <len=0001><id=0>
            CoreId::Choke => Core::Choke,

            // <len=0001><id=1>
            CoreId::Unchoke => Core::Unchoke,

            // <len=0001><id=2>
            CoreId::Interested => Core::Interested,

            // <len=0001><id=3>
            CoreId::NotInterested => Core::NotInterested,

            // <len=0005><id=4><piece index>
            CoreId::Have => {
                if buf.remaining() < 4 {
                    return Ok(None);
                }
                Core::Have(buf.get_u32() as usize)
            }

            // <len=0001+X><id=5><bitfield>
            CoreId::Bitfield => {
                let bitfield = buf.copy_to_bytes(size - 1).to_vec();
                Core::Bitfield(Bitfield::from_vec(bitfield))
            }

            // <len=0013><id=6><index><begin><length>
            CoreId::Request => {
                if buf.remaining() < 4 + 4 + 4 {
                    return Ok(None);
                }
                let index = buf.get_u32();
                let begin = buf.get_u32();
                let len = buf.get_u32();

                Core::Request(BlockInfo { index, begin, len })
            }

            // <len=0009+X><id=7><index><begin><block>
            CoreId::Piece => {
                if buf.remaining() < 4 + 4 {
                    return Ok(None);
                }
                let index = buf.get_u32() as usize;
                let begin = buf.get_u32();

                // size - 4 bytes (index) - 4 bytes (begin) - 1 byte (msg_id)
                let block = buf.copy_to_bytes(size - 9).to_vec();

                Core::Piece(Block { index, begin, block })
            }

            // <len=0013><id=8><index><begin><length>
            CoreId::Cancel => {
                if buf.remaining() < 4 + 4 + 4 {
                    return Ok(None);
                }
                let index = buf.get_u32();
                let begin = buf.get_u32();
                let len = buf.get_u32();

                Core::Cancel(BlockInfo { index, begin, len })
            }

            // <len=002 + payload><id=20><ext_id><payload>
            CoreId::Extended => {
                if buf.remaining() < 1 {
                    return Ok(None);
                }
                let ext_id = buf.get_u8();

                // size - 1 byte (msg_id) - 1 byte (ext_id)
                let payload = buf.copy_to_bytes(size - 2).to_vec();

                Core::Extended(ExtendedMessage(ext_id, payload))
            }
        };

        Ok(Some(msg))
    }
}

#[cfg(test)]
mod tests {
    use crate::extensions::{core::BLOCK_LEN, Metadata, MetadataMsgType};

    use super::*;
    use bitvec::{bitvec, prelude::Msb0};
    use bytes::{Buf, BytesMut};
    use tokio_util::codec::{Decoder, Encoder};

    #[test]
    fn fragmented_extended_message() {
        let mut codec = CoreCodec {};
        let mut buffer = BytesMut::new();

        // Create metadata with 50,000 bytes
        let metadata = vec![0xAA; 50_000];

        // For extended message:
        // Total length = 1 (msg_id) + 1 (ext_id) + 50_000 (payload) = 50,002
        // bytes
        let total_length = 50_002_u32;
        let header = total_length.to_be_bytes();

        // Build message content: [msg_id][ext_id][payload]
        let mut message_content = Vec::with_capacity(total_length as usize);
        message_content.push(CoreId::Extended as u8); // 1 byte
        message_content.push(0); // ext_id: 1 byte
        message_content.extend_from_slice(&metadata); // 50,000 bytes

        // Split into 3 chunks
        let chunk1 = &message_content[..15_002]; // First 15,002 bytes
        let chunk2 = &message_content[15_002..35_002]; // Next 20,000 bytes
        let chunk3 = &message_content[35_002..]; // Last 15,000 bytes

        // Simulate TCP fragmentation
        buffer.extend_from_slice(&header);
        buffer.extend_from_slice(chunk1);

        // First chunk should be incomplete
        assert!(codec.decode(&mut buffer).unwrap().is_none());

        // Add second chunk
        buffer.extend_from_slice(chunk2);
        assert!(codec.decode(&mut buffer).unwrap().is_none());

        // Add final chunk
        buffer.extend_from_slice(chunk3);

        let keepalive = [0x00, 0x00, 0x00, 0x00];
        let interested = [0x00, 0x00, 0x00, 0x01, 0x02]; // Length=1, ID=2 (Interested)

        buffer.extend_from_slice(&keepalive);
        buffer.extend_from_slice(&interested);

        let msg = codec.decode(&mut buffer).unwrap().unwrap();

        match msg {
            Core::Extended(ExtendedMessage(ext_id, payload)) => {
                assert_eq!(ext_id, 0);
                assert_eq!(payload.len(), 50_000);
                assert!(payload.iter().all(|&b| b == 0xAA));
            }
            _ => panic!("Wrong message type"),
        }

        // ----------------

        let msg = codec.decode(&mut buffer).unwrap().unwrap();
        assert_eq!(msg, Core::KeepAlive);

        let msg = codec.decode(&mut buffer).unwrap().unwrap();
        assert_eq!(msg, Core::Interested);

        assert!(buffer.is_empty());
        assert!(codec.decode(&mut buffer).unwrap().is_none());
    }

    #[test]
    fn from_ext_piece() {
        let bytes: [u8; _] = [
            0x00, 0x00, 0x00, 0xf8, 0x14, 0x03, 0x64, 0x38, 0x3a, 0x6d, 0x73,
            0x67, 0x5f, 0x74, 0x79, 0x70, 0x65, 0x69, 0x31, 0x65, 0x35, 0x3a,
            0x70, 0x69, 0x65, 0x63, 0x65, 0x69, 0x30, 0x65, 0x31, 0x30, 0x3a,
            0x74, 0x6f, 0x74, 0x61, 0x6c, 0x5f, 0x73, 0x69, 0x7a, 0x65, 0x69,
            0x32, 0x38, 0x32, 0x35, 0x38, 0x65, 0x65, 0x64, 0x36, 0x3a, 0x6c,
            0x65, 0x6e, 0x67, 0x74, 0x68, 0x69, 0x31, 0x34, 0x37, 0x34, 0x37,
            0x35, 0x38, 0x31, 0x33, 0x32, 0x65, 0x34, 0x3a, 0x6e, 0x61, 0x6d,
            0x65, 0x34, 0x39, 0x3a, 0x5b, 0x53, 0x75, 0x62, 0x73, 0x50, 0x6c,
            0x65, 0x61, 0x73, 0x65, 0x5d, 0x20, 0x44, 0x61, 0x6e, 0x64, 0x61,
            0x64, 0x61, 0x6e, 0x20, 0x2d, 0x20, 0x31, 0x37, 0x20, 0x28, 0x31,
            0x30, 0x38, 0x30, 0x70, 0x29, 0x20, 0x5b, 0x44, 0x43, 0x42, 0x41,
            0x34, 0x38, 0x42, 0x41, 0x5d, 0x2e, 0x6d, 0x6b, 0x76, 0x31, 0x32,
            0x3a, 0x70, 0x69, 0x65, 0x63, 0x65, 0x20, 0x6c, 0x65, 0x6e, 0x67,
            0x74, 0x68, 0x69, 0x31, 0x30, 0x34, 0x38, 0x35, 0x37, 0x36, 0x65,
            0x36, 0x3a, 0x70, 0x69, 0x65, 0x63, 0x65, 0x73, 0x32, 0x38, 0x31,
            0x34, 0x30, 0x3a, 0x78, 0xe0, 0x9b, 0x23, 0xcc, 0x37, 0x7c, 0xd0,
            0x24, 0xbc, 0xea, 0x74, 0xed, 0xa1, 0x24, 0xca, 0x21, 0x72, 0x47,
            0x69, 0x5e, 0x53, 0x2a, 0x14, 0x25, 0xb4, 0x97, 0xfb, 0x22, 0x08,
            0x0f, 0xbe, 0xb3, 0x0e, 0xa8, 0xf8, 0xc3, 0xc7, 0x50, 0xa3, 0x30,
            0xb1, 0xc2, 0x74, 0xc0, 0x89, 0xd6, 0x83, 0x27, 0xf3, 0xf2, 0xd0,
            0xa2, 0x07, 0x89, 0x95, 0x72, 0x2a, 0x9b, 0x1b, 0xe8, 0x53, 0x2d,
            0xd4, 0x22, 0xab, 0x4e, 0x7d, 0xfe, 0x0d, 0x61, 0x84, 0x3c, 0x08,
            0xa0, 0x92, 0x2e, 0x27, 0x98, 0xf8, 0xc2, 0x5f, 0x9f, 0x32,
        ];
        let mut buf = BytesMut::with_capacity(bytes.len());
        buf.extend_from_slice(&bytes);

        let ext = CoreCodec.decode(&mut buf).unwrap().unwrap();

        if let Core::Extended(msg @ ExtendedMessage(..)) = ext {
            assert_eq!(msg.0, Metadata::ID);
            let msg: Metadata = msg.try_into().unwrap();
            assert_eq!(msg.piece, 0);
            assert_eq!(msg.msg_type, MetadataMsgType::Response);
            assert_eq!(msg.total_size, Some(28258));
            assert_eq!(msg.payload.len(), 201);
        }
        assert!(buf.is_empty());
    }

    #[test]
    fn extended() {
        let mut buf = BytesMut::new();
        let msg: Core = ExtendedMessage(0, vec![]).into();
        CoreCodec.encode(msg.clone(), &mut buf).unwrap();

        // len
        assert_eq!(buf.len(), 6);
        // len prefix
        assert_eq!(buf.get_u32(), 2);
        // ext id
        assert_eq!(buf.get_u8(), CoreId::Extended as u8);
        // ext id
        assert_eq!(buf.get_u8(), 0);

        let mut buf = BytesMut::new();
        CoreCodec.encode(msg, &mut buf).unwrap();
        let msg = CoreCodec.decode(&mut buf).unwrap().unwrap();

        match msg {
            Core::Extended(ExtendedMessage(ext_id, _payload)) => {
                assert_eq!(ext_id, 0);
            }
            _ => panic!(),
        }
        assert!(buf.is_empty());
    }

    #[test]
    fn bitfield() {
        let mut original = bitvec![u8, Msb0; 0; 10];
        println!("original {original:?}");
        let original_len_bytes = original.len().div_ceil(8);

        original.set(8, true);
        original.set(9, true);

        let msg = Core::Bitfield(original.clone());

        let mut buf = BytesMut::new();
        CoreCodec.encode(msg.clone(), &mut buf).unwrap();

        // len
        assert_eq!(buf.get_u32(), 1 + original_len_bytes as u32);

        // msg_id
        assert_eq!(buf.get_u8(), CoreId::Bitfield as u8);

        // bitfield
        // we compare the capacity because if the bit len is not multiple of 8,
        // bitvec! will cut the unused bits but will keep the capacity. And when
        // we send a bitfield over to the network, it gets converted
        // with the "useless" bits.
        assert_eq!(Bitfield::from_slice(&buf).capacity(), original.capacity());
    }

    #[test]
    fn request() {
        let mut buf = BytesMut::new();
        let msg = Core::Request(BlockInfo::default());
        CoreCodec.encode(msg.clone(), &mut buf).unwrap();

        // size of buf
        assert_eq!(buf.len(), 17);
        // len
        assert_eq!(buf.get_u32(), 13);
        // id
        assert_eq!(buf.get_u8(), CoreId::Request as u8);
        // index
        assert_eq!(buf.get_u32(), 0);
        // begin
        assert_eq!(buf.get_u32(), 0);
        // len of block
        assert_eq!(buf.get_u32(), BLOCK_LEN);

        let mut buf = BytesMut::new();
        CoreCodec.encode(msg, &mut buf).unwrap();
        let msg = CoreCodec.decode(&mut buf).unwrap().unwrap();

        match msg {
            Core::Request(block_info) => {
                assert_eq!(block_info.index, 0);
                assert_eq!(block_info.begin, 0);
                assert_eq!(block_info.len, BLOCK_LEN);
            }
            _ => panic!(),
        }
        assert!(buf.is_empty());
    }

    #[test]
    fn piece() {
        let mut buf = BytesMut::new();
        let msg = Core::Piece(Block { index: 0, begin: 0, block: vec![0] });
        CoreCodec.encode(msg.clone(), &mut buf).unwrap();

        // len
        assert_eq!(buf.get_u32(), 9 + 1);
        // id
        assert_eq!(buf.get_u8(), CoreId::Piece as u8);
        // index
        assert_eq!(buf.get_u32(), 0);
        // begin
        assert_eq!(buf.get_u32(), 0);
        // block
        let mut block = BytesMut::new();
        buf.copy_to_slice(&mut block);
        assert_eq!(block.len(), 0);

        let mut buf = BytesMut::new();
        CoreCodec.encode(msg.clone(), &mut buf).unwrap();
        let msg = CoreCodec.decode(&mut buf).unwrap().unwrap();

        match msg {
            Core::Piece(block) => {
                assert_eq!(block.index, 0);
                assert_eq!(block.begin, 0);
                assert_eq!(block.block.len(), 1);
            }
            _ => panic!(),
        }
        assert!(buf.is_empty());
    }

    #[test]
    fn reserved_bytes() {
        let reserved = Bitfield::from_vec(vec![0, 0, 0, 0, 0, 16, 0, 0]);
        // let mut a = bitarr![u8, Msb0; 0; 64];
        // reserved.set(43, true);

        assert_eq!(reserved.clone().into_vec(), [0, 0, 0, 0, 0, 16, 0, 0]);

        let support_extension_protocol = reserved[43];

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
// message is a bitfield whose indices represent the file pieces in the torrent
// and is used to tell the other peer which pieces the sender has available
// (each available piece's bitfield value is 1). Byte 0 corresponds to indices
// 0-7, from most significant bit to least significant bit, respectively, byte 1
// corresponds to indices 8-15, and so on. E.g. given the first byte
// `0b1100'0001` in the bitfield means we have pieces 0, 1, and 7.
//
// If a peer doesn't have any pieces downloaded, they need not send
// this message.

// Request
// <len=0013><id=6><index><begin><length>
// This message is sent when a downloader requests a chunk of a file piece from
// its peer. It specifies the piece index, the offset into that piece, and the
// length of the block. As noted above, due to nearly all clients in the wild
// reject requests that are not 16 KiB, we can assume the length field to always
// be 16 KiB.
//
// Whether we should also reject requests for different values is an
// open-ended question, as only allowing 16 KiB blocks allows for certain
// optimizations.
// <len=0009+X><id=7><index><begin><block>
// `piece` messages are the responses to `request` messages, containing the
// request block's payload. It is possible for an unexpected piece to arrive if
// choke and unchoke messages are sent in quick succession, if transfer is going
// slowly, or both.

// Cancel
// <len=0013><id=8><index><begin><length>
// Used to cancel an outstanding download request. Generally used towards the
// end of a download in `endgame mode`.
//
// When a download is almost complete, there's a tendency for the last few
// pieces to all be downloaded off a single hosed modem line, taking a very long
// time. To make sure the last few pieces come in quickly, once requests for all
// pieces a given downloader doesn't have yet are currently pending, it sends
// requests for everything to everyone it's downloading from. To keep this from
// becoming horribly inefficient, it sends cancels to everyone else every time a
// piece arrives.
