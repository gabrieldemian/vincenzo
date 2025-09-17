use int_enum::IntEnum;

use super::*;

/// The type field describes the type of packet.
#[repr(u8)]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, IntEnum)]
pub(crate) enum PacketType {
    #[default]
    /// regular data packet. Socket is in connected state and has data to send.
    /// A Data packet always has a data payload.
    Data = 0,

    /// Finalize the connection. This is the last packet. It closes the
    /// connection, similar to TCP FIN flag. This connection will never have a
    /// sequence number greater than the sequence number in this packet. The
    /// socket records this sequence number as eof_pkt. This lets the socket
    /// wait for packets that might still be missing and arrive out of order
    /// even after receiving the ST_FIN packet.
    Fin = 1,

    /// State packet. Used to transmit an ACK with no data. Packets that don't
    /// include any payload do not increase the seq_nr.
    State = 2,

    /// Terminate connection forcefully. Similar to TCP RST flag. The remote
    /// host does not have any state for this connection. It is stale and should
    /// be terminated.
    Reset = 3,

    /// Connect SYN. Similar to TCP SYN flag, this packet initiates a
    /// connection. The sequence number is initialized to 1. The connection ID
    /// is initialized to a random number. The syn packet is special, all
    /// subsequent packets sent on this connection (except for re-sends of the
    /// SYN) are sent with the connection ID + 1. The connection ID is what
    /// the other end is expected to use in its responses.
    ///
    /// When receiving an SYN, the new socket should be initialized with the
    /// ID in the packet header. The send ID for the socket should be
    /// initialized to the ID + 1. The sequence number for the return channel is
    /// initialized to a random number. The other end expects an STATE packet
    /// (only an ACK) in response.
    Syn = 4,
}

/// UTP packet structure
#[derive(Debug)]
pub(crate) struct UtpPacket {
    pub header: Header,
    pub payload: Bytes,
}

impl From<UtpPacket> for Vec<u8> {
    fn from(value: UtpPacket) -> Self {
        let mut v = Vec::with_capacity(20 + value.payload.len());
        v.extend_from_slice(&value.header.to_bytes());
        v.extend_from_slice(&value.payload);
        v
    }
}

impl UtpPacket {
    /// Parse a byte buffer into a UtpPacket
    pub fn parse_packet(data: &[u8]) -> io::Result<Self> {
        if data.len() < 20 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Packet too short for UTP header",
            ));
        }

        let header = Header::from_bytes(&data[0..20])?;
        let payload = Bytes::copy_from_slice(&data[20..]);

        Ok(UtpPacket { header, payload })
    }
}
