use bitvec::bitvec;
use bytes::{Buf, BufMut, BytesMut};
use futures::SinkExt;
use std::{io::Cursor, sync::atomic::Ordering};
use tokio::{io, sync::oneshot};
use tokio_util::codec::{Decoder, Encoder};
use tracing::{debug, info, trace, warn};
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
    Choke,
    Unchoke,
    Interested,
    NotInterested,
    Have(usize),
    Bitfield(Bitfield),
    Request(BlockInfo),
    Piece(Block),
    Cancel(BlockInfo),
    Extended(ExtendedMessage),
    KeepAlive,
}

/// The IDs of the [`Core`] messages.
#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq)]
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

impl TryFrom<u8> for CoreId {
    type Error = io::Error;

    fn try_from(k: u8) -> Result<Self, Self::Error> {
        use CoreId::*;
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
        let remote = peer.state.ctx.remote_addr;

        match msg {
            // handled by the extended messages
            Core::Extended(_) => {}
            Core::KeepAlive => {
                debug!("{remote} keepalive");
            }
            Core::Bitfield(bitfield) => {
                // take entire pieces from bitfield
                // and put in pending_requests
                info!("{remote} bitfield len {:?}", bitfield.len());
                info!("{remote} bitfield {bitfield:?}");

                let b = &mut peer.state.pieces;
                *b = bitfield.clone();

                debug!("{remote} bitfield is len {:?}", bitfield.len());
            }
            Core::Unchoke => {
                peer.state.ctx.peer_choking.store(false, Ordering::Relaxed);
                info!("{remote} unchoke");
            }
            Core::Choke => {
                // remote peer is choking the local peer
                info!("{remote} choke");
                peer.state.ctx.peer_choking.store(true, Ordering::Relaxed);
                peer.free_pending_blocks().await;
            }
            Core::Interested => {
                // remote peer is interested the local peer
                debug!("{remote} interested");
                peer.state.ctx.peer_interested.store(true, Ordering::Relaxed);
            }
            Core::NotInterested => {
                debug!("{remote} not_interested");
                peer.state.ctx.peer_interested.store(false, Ordering::Relaxed);
            }
            Core::Have(piece) => {
                debug!("{remote} have {piece}");

                // Overwrite pieces on bitfield, if the peer has one
                let peer_pieces = &mut peer.state.pieces;

                let (otx, orx) = oneshot::channel();

                peer.state
                    .torrent_ctx
                    .tx
                    .send(TorrentMsg::ReadBitfield(otx))
                    .await?;

                // local bitfield of the local peer
                let local_pieces = orx.await?;

                // peer sent a piece which is out of bounds with it's pieces
                if peer_pieces.get(piece).is_none() {
                    warn!(
                        "{remote} sent have but it's bitfield is out of
bounds"
                    );
                    warn!(
                        "initializing an empty bitfield with the len of the
piece {piece}"
                    );

                    // if local peer has full info, calc the difference of the
                    // bitfields and just append the difference to the peer's
                    if peer.state.have_info {
                        let missing_bits =
                            local_pieces.len() - peer_pieces.len();
                        peer_pieces
                            .extend_from_bitslice(&bitvec![0; missing_bits]);
                    } else {
                        let missing_bits = piece - peer_pieces.len();
                        peer_pieces
                            .extend_from_bitslice(&bitvec![0; missing_bits]);
                    }
                }

                peer_pieces.set(piece, true);
            }
            Core::Piece(block) => {
                info!("{remote} sent piece i: {}", block.index);
                debug!(
                    "index: {:?}, begin: {:?}, len: {:?}",
                    block.index,
                    block.begin,
                    block.block.len()
                );

                peer.state
                    .ctx
                    .uploaded
                    .fetch_add(block.block.len() as u64, Ordering::Relaxed);

                peer.handle_piece_msg(block).await?;
            }
            Core::Cancel(block_info) => {
                debug!("{remote} cancel from");
                debug!("{block_info:?}");
                peer.state.incoming_requests.retain(|v| *v != block_info);
            }
            Core::Request(block_info) => {
                debug!("{remote} request from");
                debug!("{block_info:?}");

                if peer.state.ctx.peer_choking.load(Ordering::Relaxed) {
                    return Ok(());
                }

                let (tx, rx) = oneshot::channel();

                peer.state.incoming_requests.push(block_info.clone());

                peer.state
                    .torrent_ctx
                    .disk_tx
                    .send(DiskMsg::ReadBlock {
                        block_info,
                        recipient: tx,
                        info_hash: peer.state.torrent_ctx.info_hash.clone(),
                    })
                    .await?;

                let block = rx.await?;

                peer.state
                    .ctx
                    .downloaded
                    .fetch_add(block.block.len() as u64, Ordering::Relaxed);

                let _ = peer.state.sink.send(Core::Piece(block)).await;
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
                return Ok(Some(Core::KeepAlive));
            }
        } else {
            trace!(
                "Read buffer is {} bytes long but message is {} bytes long",
                buf.remaining(),
                msg_len
            );
            return Ok(None);
        }

        let msg_id = CoreId::try_from(buf.get_u8())?;

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
                let piece_index = buf.get_u32();
                Core::Have(piece_index as usize)
            }
            // <len=0001+X><id=5><bitfield>
            CoreId::Bitfield => {
                let mut bitfield = vec![0; msg_len - 1];
                buf.copy_to_slice(&mut bitfield);
                Core::Bitfield(Bitfield::from_vec(bitfield))
            }
            // <len=0013><id=6><index><begin><length>
            CoreId::Request => {
                let index = buf.get_u32();
                let begin = buf.get_u32();
                let len = buf.get_u32();

                Core::Request(BlockInfo { index, begin, len })
            }
            // <len=0009+X><id=7><index><begin><block>
            CoreId::Piece => {
                let index = buf.get_u32() as usize;
                let begin = buf.get_u32();

                let mut block = vec![0; msg_len - 9];
                buf.copy_to_slice(&mut block);

                Core::Piece(Block { index, begin, block })
            }
            // <len=0013><id=8><index><begin><length>
            CoreId::Cancel => {
                let index = buf.get_u32();
                let begin = buf.get_u32();
                let len = buf.get_u32();

                Core::Cancel(BlockInfo { index, begin, len })
            }
            // <len=002 + payload><id=20><ext_id><payload>
            CoreId::Extended => {
                let ext_id = buf.get_u8();

                let mut payload = vec![0u8; msg_len - 2];
                buf.copy_to_slice(&mut payload);

                Core::Extended(ExtendedMessage(ext_id, payload))
            }
        };

        Ok(Some(msg))
    }
}

#[cfg(test)]
mod tests {
    use crate::extensions::core::BLOCK_LEN;

    use super::*;
    use bitvec::{bitvec, prelude::Msb0};
    use bytes::{Buf, BytesMut};
    use tokio_util::codec::{Decoder, Encoder};

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
    }

    #[test]
    fn bitfield() {
        let mut buf = BytesMut::new();
        let mut original = bitvec![u8, Msb0; 0; 10];
        original.set(8, true);
        original.set(9, true);
        // let original = Bitfield::from_vec(vec![255]);
        let msg = Core::Bitfield(original.clone());

        CoreCodec.encode(msg.clone(), &mut buf).unwrap();

        // len
        assert_eq!(buf.get_u32(), 1 + original.clone().into_vec().len() as u32);
        // ext id
        assert_eq!(buf.get_u8(), CoreId::Bitfield as u8);

        let mut buf = BytesMut::new();
        CoreCodec.encode(msg, &mut buf).unwrap();
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
