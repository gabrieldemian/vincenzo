//! Micro Transport Protocol (μTP)

mod header;
pub(crate) use header::*;

mod packet;
pub(crate) use packet::*;

use std::{
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicU16, AtomicU32, AtomicU64, Ordering},
    },
    time::{Instant, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::AsyncWrite,
    net::{ToSocketAddrs, UdpSocket, lookup_host},
};
use tokio_util::bytes::{Buf, BufMut, Bytes, BytesMut};

fn current_timestamp() -> u32 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_micros() as u32
}

/// UTP protocol version (currently version 1)
pub(crate) const UTP_VERSION: u8 = 1;

/// Connection state for UTP socket
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum ConnectionState {
    SynSent,
    Connected,
    Closing,
    Closed,
}

pub struct UtpSocket {
    udp_socket: Arc<UdpSocket>,

    /// This is a header manager that keeps track of internal data to create
    /// new headers.
    utp_header: UtpHeader,

    state: ConnectionState,
    connection_id: AtomicU16,

    /// The number of bytes in-flight.
    cur_window: AtomicU32,

    /// Maximum window that the socket *may* enforce.
    ///
    /// A socket may only send a packet if cur_window + packet_size is less
    /// than or equal to min(max_window, wnd_size).
    ///
    /// An implementation MAY violate the above rule if the max_window is
    /// smaller than the packet size, and it paces the packets so that the
    /// average cur_window is less than or equal to max_window.
    max_window: AtomicU32,

    buf: Vec<u8>,

    // round trip time as microseconds.
    rtt: AtomicU64,
    last_packet_time: Instant,

    connect_notify: tokio::sync::Notify,
    recv_notify: tokio::sync::Notify,
}

impl AsyncWrite for UtpSocket {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<Result<usize, std::io::Error>> {
        todo!();
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), std::io::Error>> {
        todo!();
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), std::io::Error>> {
        todo!();
    }
}

impl UtpSocket {
    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.udp_socket.local_addr()
    }

    /// Creates a new UTP socket bound to the specified address
    pub async fn bind<A: ToSocketAddrs>(addr: A) -> std::io::Result<Self> {
        let udp_socket = Arc::new(UdpSocket::bind(addr).await?);

        Ok(UtpSocket {
            udp_socket,
            buf: Vec::new(),
            utp_header: UtpHeader::new(0.into(), 0),
            state: ConnectionState::Closed,
            connection_id: 0.into(),
            cur_window: 0.into(),
            max_window: 10_000.into(),
            // recv_buffer: VecDeque::new(),
            // default to 100ms in microseconds
            rtt: AtomicU64::new(100_000),
            last_packet_time: Instant::now(),
            connect_notify: tokio::sync::Notify::new(),
            recv_notify: tokio::sync::Notify::new(),
        })
    }

    /// Connects to a remote UTP endpoint
    pub async fn connect<A: ToSocketAddrs>(
        &mut self,
        addr: A,
    ) -> std::io::Result<()> {
        let remote_addr = lookup_host(addr).await?.next().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "No address provided",
            )
        })?;

        self.udp_socket.connect(remote_addr).await?;
        self.connection_id.store(rand::random::<u16>(), Ordering::SeqCst);

        self.send_syn().await?;
        self.state = ConnectionState::SynSent;
        self.connect_notify.notified().await;

        Ok(())
    }

    /// Sends a SYN packet to initiate connection
    async fn send_syn(&mut self) -> std::io::Result<()> {
        let header = self.utp_header.new_syn();
        let packet = UtpHeader::build_packet(header, Bytes::new());
        self.send_packet(&packet.into()).await
    }

    /// Sends a packet through the underlying UDP socket
    async fn send_packet(&mut self, packet: &Bytes) -> std::io::Result<()> {
        self.udp_socket.send(packet).await?;
        self.last_packet_time = Instant::now();
        Ok(())
    }

    /// Processes incoming UTP packets
    async fn process_packet(&self, packet: UtpPacket) -> std::io::Result<()> {
        match packet.header.type_ver.packet_type()? {
            PacketType::Syn => self.handle_syn(packet).await,
            PacketType::Data => self.handle_data(packet).await,
            PacketType::State => self.handle_state(packet).await,
            PacketType::Fin => self.handle_fin(packet).await,
            PacketType::Reset => self.handle_reset(packet).await,
        }
    }

    /// Handles SYN packets (connection initiation)
    async fn handle_syn(&self, packet: UtpPacket) -> std::io::Result<()> {
        // Implementation for SYN handling
        Ok(())
    }

    /// Handles DATA packets
    async fn handle_data(&self, packet: UtpPacket) -> std::io::Result<()> {
        // let mut recv_buffer = self.recv_buffer.lock().await;
        // recv_buffer.push_back(packet);
        Ok(())
    }

    async fn handle_fin(&self, packet: UtpPacket) -> std::io::Result<()> {
        todo!()
    }

    async fn handle_state(&self, packet: UtpPacket) -> std::io::Result<()> {
        todo!()
    }

    async fn handle_reset(&self, packet: UtpPacket) -> std::io::Result<()> {
        todo!()
    }

    fn cur_window(&self) -> u32 {
        self.cur_window.load(Ordering::Relaxed)
    }

    pub fn remote_addr(&self) -> std::io::Result<SocketAddr> {
        self.udp_socket.peer_addr()
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::AsyncWriteExt;

    use super::*;

    #[tokio::test]
    async fn test_bind_and_send() {
        let mut sender = UtpSocket::bind("127.0.0.1:34254")
            .await
            .expect("Failed to bind UTP socket");

        let mut receiver = UtpSocket::bind("127.0.0.1:34255")
            .await
            .expect("Failed to bind UDP socket");

        sender
            .connect(receiver.local_addr().unwrap())
            .await
            .expect("Failed to connect");

        let test_payload = Bytes::from("test data");

        tokio::spawn(async move {
            let bytes_sent = sender.write_u8(7).await;
        });
    }
}
