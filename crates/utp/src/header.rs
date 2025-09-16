//! Header of the protocol.

use super::*;

/// 0       4       8               16              24              32
/// +-------+-------+---------------+---------------+---------------+
/// | type  | ver   | extension     | connection_id                 |
/// +-------+-------+---------------+---------------+---------------+
/// | timestamp_microseconds                                        |
/// +---------------+---------------+---------------+---------------+
/// | timestamp_difference_microseconds                             |
/// +---------------+---------------+---------------+---------------+
/// | wnd_size                                                      |
/// +---------------+---------------+---------------+---------------+
/// | seq_nr                        | ack_nr                        |
/// +---------------+---------------+---------------+---------------+
#[derive(Debug)]
pub(crate) struct Header {
    pub type_ver: TypeVer,

    /// The type of the first extension in a linked list of extension headers.
    /// 0 means no extension.
    extension: u8,
    connection_id: u16,

    /// The timestamp of when this packet was sent.
    timestamp_microseconds: u32,

    /// the difference between the local time and the timestamp in the last
    /// received packet, at the time the last packet was received. This is the
    /// latest one-way delay measurement of the link from the remote peer to
    /// the local machine.
    timestamp_difference_microseconds: u32,
    wnd_size: u32,
    seq_nr: u16,
    ack_nr: u16,
}

impl Header {
    pub fn to_bytes(&self) -> [u8; 20] {
        let mut buf = [0u8; 20];
        buf[0] = self.type_ver.0;
        buf[1] = self.extension;
        buf[2..4].copy_from_slice(&self.connection_id.to_be_bytes());
        buf[4..8].copy_from_slice(&self.timestamp_microseconds.to_be_bytes());
        buf[8..12].copy_from_slice(
            &self.timestamp_difference_microseconds.to_be_bytes(),
        );
        buf[12..16].copy_from_slice(&self.wnd_size.to_be_bytes());
        buf[16..18].copy_from_slice(&self.seq_nr.to_be_bytes());
        buf[18..20].copy_from_slice(&self.ack_nr.to_be_bytes());
        buf
    }

    pub fn from_bytes(buf: &[u8]) -> Option<Self> {
        if buf.len() < 20 {
            return None;
        }

        Some(Header {
            type_ver: TypeVer(buf[0]),
            extension: buf[1],
            connection_id: u16::from_be_bytes([buf[2], buf[3]]),
            timestamp_microseconds: u32::from_be_bytes([
                buf[4], buf[5], buf[6], buf[7],
            ]),
            timestamp_difference_microseconds: u32::from_be_bytes([
                buf[8], buf[9], buf[10], buf[11],
            ]),
            wnd_size: u32::from_be_bytes([buf[12], buf[13], buf[14], buf[15]]),
            seq_nr: u16::from_be_bytes([buf[16], buf[17]]),
            ack_nr: u16::from_be_bytes([buf[18], buf[19]]),
        })
    }
}

#[derive(Debug)]
pub(crate) struct TypeVer(pub(crate) u8);

impl TypeVer {
    pub fn version(&self) -> u8 {
        self.0 & 0x0F
    }

    pub fn packet_type(&self) -> std::io::Result<PacketType> {
        let type_bits = (self.0 >> 4) & 0x0F;
        type_bits
            .try_into()
            .map_err(|e| std::io::Error::other(format!("wrong type_ver: {e}")))
    }

    pub fn from_packet(packet: PacketType) -> Self {
        Self(UTP_VERSION | (packet as u8) << 4)
    }

    pub fn set_packet_type(&mut self, packet_type: PacketType) {
        self.0 = ((packet_type as u8) << 4) | (self.0 & 0x0F);
    }
}

impl std::fmt::Display for TypeVer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "TypeVer({:02x}) - Type: {:?}, Version: {}",
            self.0,
            self.packet_type(),
            self.version()
        )
    }
}

#[derive(Debug)]
pub(crate) struct UtpHeader {
    /// Advertised receive window
    wnd_size: u32,
    connection_id: AtomicU16,

    /// This is the sequence number of this packet.
    seq_nr: AtomicU16,

    /// The sequence number the sender of the packet last received in
    /// the other direction.
    ack_nr: AtomicU16,
}

impl UtpHeader {
    pub(crate) fn new(connection_id: AtomicU16, wnd_size: u32) -> Self {
        UtpHeader {
            connection_id,
            wnd_size,
            seq_nr: 0.into(),
            ack_nr: 0.into(),
        }
    }

    /// Create a new header with the packet type syn.
    pub(crate) fn new_syn(&self) -> Header {
        Header {
            ack_nr: self.ack_nr(),
            connection_id: self.connection_id(),
            type_ver: TypeVer::from_packet(PacketType::Data),
            seq_nr: self.next_seq_nr(),
            wnd_size: self.wnd_size,
            ..Default::default()
        }
    }

    /// Create a new header with the packet type data.
    pub(crate) fn new_data(&self) -> Header {
        Header {
            ack_nr: self.ack_nr(),
            connection_id: self.connection_id(),
            type_ver: TypeVer::from_packet(PacketType::Data),
            seq_nr: self.next_seq_nr(),
            wnd_size: self.wnd_size,
            ..Default::default()
        }
    }

    /// Builds a UTP packet from header and payload
    pub(crate) fn build_packet(header: Header, payload: Bytes) -> BytesMut {
        let mut buf = BytesMut::with_capacity(20 + payload.len());

        buf.put_u8(header.type_ver.0);
        buf.put_u8(header.extension);
        buf.put_u16(header.connection_id);
        buf.put_u32(header.timestamp_microseconds);
        buf.put_u32(header.timestamp_difference_microseconds);
        buf.put_u32(header.wnd_size);
        buf.put_u16(header.seq_nr);
        buf.put_u16(header.ack_nr);

        buf.extend_from_slice(&payload);
        buf
    }

    /// Parses incoming UDP data into UTP packet
    pub(crate) fn parse_packet(mut buf: &[u8]) -> std::io::Result<UtpPacket> {
        if buf.len() < 20 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Packet too short",
            ));
        }

        let type_ver = buf.get_u8();
        let _packet_type = match (type_ver >> 4) & 0x0F {
            0 => PacketType::Data,
            1 => PacketType::Fin,
            2 => PacketType::State,
            3 => PacketType::Reset,
            4 => PacketType::Syn,
            _ => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "Invalid packet type",
                ));
            }
        };

        let version = type_ver & 0x0F;
        if version != UTP_VERSION {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Unsupported UTP version",
            ));
        }

        let extension = buf.get_u8();
        let connection_id = buf.get_u16();
        let timestamp_microseconds = buf.get_u32();
        let timestamp_difference_microseconds = buf.get_u32();
        let wnd_size = buf.get_u32();
        let seq_nr = buf.get_u16();
        let ack_nr = buf.get_u16();

        let payload = Bytes::copy_from_slice(buf);

        Ok(UtpPacket {
            header: Header {
                type_ver: TypeVer(type_ver),
                extension,
                connection_id,
                timestamp_microseconds,
                timestamp_difference_microseconds,
                wnd_size,
                seq_nr,
                ack_nr,
            },
            payload,
        })
    }

    /// Gets current timestamp in microseconds
    fn seq_nr(&self) -> u16 {
        self.seq_nr.load(Ordering::Relaxed)
    }

    fn ack_nr(&self) -> u16 {
        self.ack_nr.load(Ordering::Relaxed)
    }

    fn connection_id(&self) -> u16 {
        self.connection_id.load(Ordering::Relaxed)
    }

    fn next_seq_nr(&self) -> u16 {
        self.seq_nr.fetch_add(1, Ordering::SeqCst)
    }
}

impl Default for Header {
    fn default() -> Self {
        Self {
            type_ver: TypeVer::from_packet(PacketType::Syn),
            extension: 0,
            connection_id: rand::random::<u16>(),
            timestamp_microseconds: current_timestamp(),
            timestamp_difference_microseconds: 0,
            wnd_size: 10_000,
            seq_nr: 0,
            ack_nr: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_ver_creation() {
        let type_ver = TypeVer::from_packet(PacketType::Syn);
        assert_eq!(type_ver.packet_type().unwrap(), PacketType::Syn);
        assert_eq!(type_ver.version(), 1);
        assert_eq!(type_ver.0, 0x41); // 4 (Syn) << 4 | 1 = 0x41

        let type_ver = TypeVer::from_packet(PacketType::Data);
        assert_eq!(type_ver.packet_type().unwrap(), PacketType::Data);
        assert_eq!(type_ver.version(), 1);
        assert_eq!(type_ver.0, 0x01);

        let type_ver = TypeVer::from_packet(PacketType::Reset);
        assert_eq!(type_ver.packet_type().unwrap(), PacketType::Reset);
        assert_eq!(type_ver.version(), 1);
        assert_eq!(type_ver.0, 0x31);

        let type_ver = TypeVer::from_packet(PacketType::Fin);
        assert_eq!(type_ver.packet_type().unwrap(), PacketType::Fin);
        assert_eq!(type_ver.version(), 1);
        assert_eq!(type_ver.0, 0x11);

        let type_ver = TypeVer::from_packet(PacketType::State);
        assert_eq!(type_ver.packet_type().unwrap(), PacketType::State);
        assert_eq!(type_ver.version(), 1);
        assert_eq!(type_ver.0, 0x21);
    }

    #[test]
    fn type_packet_modification() {
        let mut type_ver = TypeVer::from_packet(PacketType::Data);
        assert_eq!(type_ver.packet_type().unwrap(), PacketType::Data);
        assert_eq!(type_ver.version(), 1);

        type_ver.set_packet_type(PacketType::Fin);
        assert_eq!(type_ver.packet_type().unwrap(), PacketType::Fin);
        assert_eq!(type_ver.version(), 1);

        type_ver.set_packet_type(PacketType::Reset);
        assert_eq!(type_ver.packet_type().unwrap(), PacketType::Reset);
        assert_eq!(type_ver.version(), 1);

        type_ver.set_packet_type(PacketType::Syn);
        assert_eq!(type_ver.packet_type().unwrap(), PacketType::Syn);
        assert_eq!(type_ver.version(), 1);

        type_ver.set_packet_type(PacketType::State);
        assert_eq!(type_ver.packet_type().unwrap(), PacketType::State);
        assert_eq!(type_ver.version(), 1);

        type_ver.set_packet_type(PacketType::Data);
        assert_eq!(type_ver.packet_type().unwrap(), PacketType::Data);
        assert_eq!(type_ver.version(), 1);
    }

    #[test]
    fn header_serialization() {
        let header = Header {
            type_ver: TypeVer::from_packet(PacketType::Data),
            extension: 0,
            connection_id: 12345,
            timestamp_microseconds: 1000000,
            timestamp_difference_microseconds: 500000,
            wnd_size: 10000,
            seq_nr: 1,
            ack_nr: 0,
        };

        let bytes = header.to_bytes();
        let reconstructed = Header::from_bytes(&bytes).unwrap();

        assert_eq!(reconstructed.type_ver.0, header.type_ver.0);
        assert_eq!(reconstructed.connection_id, header.connection_id);
        assert_eq!(
            reconstructed.timestamp_microseconds,
            header.timestamp_microseconds
        );
        assert_eq!(reconstructed.wnd_size, header.wnd_size);
        assert_eq!(reconstructed.seq_nr, header.seq_nr);
        assert_eq!(reconstructed.ack_nr, header.ack_nr);
    }
}
