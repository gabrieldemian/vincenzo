//! Header of the protocol.

use super::*;

/// UTP protocol version (currently version 1)
pub(crate) const UTP_VERSION: u8 = 1;

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

    pub conn_id: u16,

    /// The timestamp of when this packet was sent.
    timestamp: u32,

    /// the difference between the local time and the timestamp in the last
    /// received packet, at the time the last packet was received. This is the
    /// latest one-way delay measurement of the link from the remote peer to
    /// the local machine.
    timestamp_diff: u32,
    wnd_size: u32,
    pub seq_nr: u16,
    pub ack_nr: u16,
}

impl Header {
    pub fn to_bytes(&self) -> [u8; 20] {
        let mut buf = [0u8; 20];
        buf[0] = self.type_ver.0;
        buf[1] = self.extension;
        buf[2..4].copy_from_slice(&self.conn_id.to_be_bytes());
        buf[4..8].copy_from_slice(&self.timestamp.to_be_bytes());
        buf[8..12].copy_from_slice(&self.timestamp_diff.to_be_bytes());
        buf[12..16].copy_from_slice(&self.wnd_size.to_be_bytes());
        buf[16..18].copy_from_slice(&self.seq_nr.to_be_bytes());
        buf[18..20].copy_from_slice(&self.ack_nr.to_be_bytes());
        buf
    }

    pub(crate) fn to_bytes_mut(self) -> BytesMut {
        let mut buf = BytesMut::with_capacity(20);
        buf.put_u8(self.type_ver.0);
        buf.put_u8(self.extension);
        buf.put_u16(self.conn_id);
        buf.put_u32(self.timestamp);
        buf.put_u32(self.timestamp_diff);
        buf.put_u32(self.wnd_size);
        buf.put_u16(self.seq_nr);
        buf.put_u16(self.ack_nr);
        buf
    }

    pub fn from_bytes(buf: &[u8]) -> io::Result<Self> {
        if buf.len() != 20 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Packet too short",
            ));
        }

        Ok(Header {
            type_ver: TypeVer(buf[0]),
            extension: buf[1],
            conn_id: u16::from_be_bytes([buf[2], buf[3]]),
            timestamp: u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]),
            timestamp_diff: u32::from_be_bytes([
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

    /// This is the sequence number of this packet.
    seq_nr: AtomicU16,

    /// The sequence number the sender of the packet last received in
    /// the other direction.
    ack_nr: AtomicU16,

    /// This is a random, unique, number identifying all the packets that
    /// belong to the same connection. Each socket has one connection ID for
    /// sending packets and a different connection ID for receiving packets.
    /// The endpoint initiating the connection decides which ID to use
    conn_id_send: AtomicU16,
    conn_id_recv: AtomicU16,

    /// Timestamp in microseconds
    last_recv_packet_timestamp: u32,
    last_sent_packet_timestamp: u32,
}

impl UtpHeader {
    pub(crate) fn new(wnd_size: u32) -> Self {
        UtpHeader {
            conn_id_send: 0.into(),
            conn_id_recv: 0.into(),
            wnd_size,
            seq_nr: 0.into(),
            ack_nr: 0.into(),
            last_recv_packet_timestamp: 0,
            last_sent_packet_timestamp: 0,
        }
    }

    /// Create a new header with the packet type syn.
    pub(crate) fn new_syn(&mut self) -> Header {
        self.conn_id_recv.store(rand::random::<u16>(), Ordering::SeqCst);
        let t = self.conn_id_recv.load(Ordering::Relaxed) + 1;
        self.conn_id_send.store(t, Ordering::SeqCst);

        let now = current_timestamp();
        self.last_sent_packet_timestamp = now;

        Header {
            ack_nr: self.ack_nr(),
            seq_nr: self.next_seq_nr(),
            conn_id: self.conn_id_recv(),
            type_ver: TypeVer::from_packet(PacketType::Syn),
            wnd_size: self.wnd_size,
            timestamp: now,
            ..Default::default()
        }
    }

    pub(crate) fn new_fin(&mut self) -> Header {
        let timestamp = current_timestamp();
        self.last_sent_packet_timestamp = timestamp;
        Header {
            ack_nr: self.ack_nr(),
            conn_id: self.conn_id_send(),
            type_ver: TypeVer::from_packet(PacketType::Fin),
            seq_nr: self.next_seq_nr(),
            wnd_size: self.wnd_size,
            timestamp,
            timestamp_diff: self.get_timestamp_diff(timestamp),
            ..Default::default()
        }
    }

    /// Create a new header with the packet type data.
    pub(crate) fn new_data(&mut self) -> Header {
        let timestamp = current_timestamp();
        self.last_sent_packet_timestamp = timestamp;

        Header {
            seq_nr: self.next_seq_nr(),
            ack_nr: self.ack_nr(),
            conn_id: self.conn_id_send(),
            type_ver: TypeVer::from_packet(PacketType::Data),
            wnd_size: self.wnd_size,
            timestamp,
            timestamp_diff: self.get_timestamp_diff(timestamp),
            ..Default::default()
        }
    }

    pub(crate) fn new_state(&mut self) -> Header {
        let timestamp = current_timestamp();
        self.last_sent_packet_timestamp = timestamp;

        Header {
            seq_nr: self.next_seq_nr(),
            ack_nr: self.ack_nr(),
            conn_id: self.conn_id_send(),
            type_ver: TypeVer::from_packet(PacketType::State),
            wnd_size: self.wnd_size,
            timestamp,
            timestamp_diff: self.get_timestamp_diff(timestamp),
            ..Default::default()
        }
    }

    pub(crate) fn new_reset(&mut self, seq_nr: u16) -> Header {
        let timestamp = current_timestamp();
        self.last_sent_packet_timestamp = timestamp;

        Header {
            conn_id: self.conn_id_send(),
            type_ver: TypeVer::from_packet(PacketType::Reset),
            seq_nr: self.seq_nr(),
            wnd_size: self.wnd_size,
            ack_nr: seq_nr,
            timestamp,
            timestamp_diff: self.get_timestamp_diff(timestamp),
            ..Default::default()
        }
    }

    fn seq_nr(&self) -> u16 {
        self.seq_nr.load(Ordering::SeqCst)
    }

    fn ack_nr(&self) -> u16 {
        self.ack_nr.load(Ordering::SeqCst)
    }

    pub(crate) fn set_ack_nr(&self, ack_nr: u16) {
        self.ack_nr.store(ack_nr, Ordering::SeqCst);
    }

    pub(crate) fn set_seq_nr(&self, seq_nr: u16) {
        self.seq_nr.store(seq_nr, Ordering::SeqCst);
    }

    fn conn_id_recv(&self) -> u16 {
        self.conn_id_recv.load(Ordering::SeqCst)
    }

    fn conn_id_send(&self) -> u16 {
        self.conn_id_recv.load(Ordering::SeqCst)
    }

    fn next_seq_nr(&self) -> u16 {
        self.seq_nr.fetch_add(1, Ordering::SeqCst);
        self.seq_nr.load(Ordering::SeqCst)
    }

    pub(crate) fn handle_recv_syn(&mut self, header: &Header) {
        self.last_recv_packet_timestamp = header.timestamp;
        self.conn_id_send.store(header.conn_id, Ordering::SeqCst);
        self.conn_id_recv.store(header.conn_id + 1, Ordering::SeqCst);
        self.set_seq_nr(rand::random());
        self.set_ack_nr(header.seq_nr);
    }

    pub(crate) fn handle_recv_state(&mut self, header: &Header) {
        self.last_recv_packet_timestamp = header.timestamp;
        self.set_ack_nr(header.seq_nr);
    }

    pub(crate) fn handle_recv_data(&mut self, header: &Header) {
        self.last_recv_packet_timestamp = header.timestamp;
        self.set_ack_nr(header.seq_nr);
    }

    fn get_timestamp_diff(&self, now: u32) -> u32 {
        now.saturating_sub(self.last_recv_packet_timestamp)
    }
}

impl Default for Header {
    fn default() -> Self {
        Self {
            type_ver: TypeVer::from_packet(PacketType::Syn),
            extension: 0,
            conn_id: rand::random::<u16>(),
            timestamp: current_timestamp(),
            timestamp_diff: 0,
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
    fn header_serialization() {
        let header = Header {
            type_ver: TypeVer::from_packet(PacketType::Data),
            extension: 0,
            conn_id: 12345,
            timestamp: 1000000,
            timestamp_diff: 500000,
            wnd_size: 10000,
            seq_nr: 1,
            ack_nr: 0,
        };

        let bytes = header.to_bytes();
        let reconstructed = Header::from_bytes(&bytes).unwrap();

        assert_eq!(reconstructed.type_ver.0, header.type_ver.0);
        assert_eq!(reconstructed.conn_id, header.conn_id);
        assert_eq!(reconstructed.timestamp, header.timestamp);
        assert_eq!(reconstructed.wnd_size, header.wnd_size);
        assert_eq!(reconstructed.seq_nr, header.seq_nr);
        assert_eq!(reconstructed.ack_nr, header.ack_nr);
    }
}
