//! Micro Transport Protocol (μTP)

mod header;
pub(crate) use header::*;

mod packet;
pub(crate) use packet::*;

use std::{
    io::{self},
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicU16, AtomicU32, AtomicU64, Ordering},
    },
    task::Poll,
    time::{Instant, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::{ToSocketAddrs, UdpSocket, lookup_host},
    sync::mpsc,
};
use tokio_util::bytes::{Buf, BufMut, Bytes, BytesMut};

/// UTP protocol version (currently version 1)
pub(crate) const UTP_VERSION: u8 = 1;

/// Connection state for UTP socket
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum ConnectionState {
    SynSent,
    SynRecv,
    Connected,
    Closing,
    Closed,
}

pub struct UtpSocket {
    control_tx: mpsc::Sender<BytesMut>,
    control_rx: mpsc::Receiver<BytesMut>,

    udp_socket: Arc<UdpSocket>,

    /// This is a header manager that keeps track of internal data to create
    /// new headers.
    utp_header: UtpHeader,

    state: ConnectionState,

    // io: PollEvented<mio::net::UdpSocket>,
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

    buf: BytesMut,

    // round trip time as microseconds.
    rtt: AtomicU64,
    last_packet_time: Instant,
}

impl AsyncWrite for UtpSocket {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, std::io::Error>> {
        if buf.is_empty() {
            return Poll::Ready(Ok(0));
        }
        self.buf.copy_from_slice(buf);
        Poll::Ready(Ok(buf.len()))
    }

    /// Gather the bytes in the buffer and send a UTP packet of type data.
    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), std::io::Error>> {
        while let Poll::Ready(Some(mut payload)) = self.control_rx.poll_recv(cx)
        {
            match self.udp_socket.poll_send(cx, &payload) {
                Poll::Ready(Ok(n)) => {
                    if n == payload.len() {
                        self.last_packet_time = Instant::now();
                    } else {
                        payload.advance(n);
                    }
                }
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => break,
            }
        }

        if self.buf.is_empty() {
            return Poll::Ready(Ok(()));
        }

        let header = self.utp_header.new_data();
        let payload = std::mem::take(&mut self.buf).freeze();
        let packet: Vec<u8> = UtpPacket { header, payload }.into();

        match self.udp_socket.poll_send(cx, &packet) {
            Poll::Ready(Ok(n)) => {
                if n == self.buf.len() {
                    self.buf.clear();
                    self.last_packet_time = Instant::now();
                    Poll::Ready(Ok(()))
                } else {
                    self.buf.advance(n);
                    cx.waker().wake_by_ref();
                    Poll::Pending
                }
            }
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Pending => Poll::Pending,
        }
    }

    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), std::io::Error>> {
        self.state = ConnectionState::Closing;

        match self.as_mut().poll_flush(cx) {
            Poll::Ready(Ok(_)) => match self.udp_socket.poll_send(cx, &[8]) {
                Poll::Pending => Poll::Pending,
                Poll::Ready(Err(e)) => {
                    self.state = ConnectionState::Closed;
                    Poll::Ready(Err(e))
                }
                Poll::Ready(Ok(_)) => {
                    self.state = ConnectionState::Closed;
                    Poll::Ready(Ok(()))
                }
            },
            Poll::Ready(Err(e)) => {
                self.state = ConnectionState::Closed;
                Poll::Ready(Err(e))
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl AsyncRead for UtpSocket {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        if !self.buf.is_empty() {
            let recv_len = self.buf.len();
            let copy_len = std::cmp::min(recv_len, buf.remaining());

            // copy data from our buffer to the user's buffer
            buf.put_slice(&self.buf[..copy_len]);

            // advance our buffer to remove the copied data
            self.buf.advance(copy_len);
            cx.waker().wake_by_ref();
            return Poll::Ready(Ok(()));
        }

        match self.try_receive_packet(cx) {
            Ok(Some(packet)) => {
                self.buf.extend_from_slice(&packet.payload);
                let _ = self.control_tx.try_send(packet.header.to_bytes_mut());

                // now try to read again
                cx.waker().wake_by_ref();
                Poll::Pending
            }
            Ok(None) => {
                // no packet available, register for wakeup
                cx.waker().wake_by_ref();
                Poll::Pending
            }
            Err(e) => Poll::Ready(Err(e)),
        }
    }
}

impl UtpSocket {
    fn try_receive_packet(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> io::Result<Option<UtpPacket>> {
        let mut temp_buf = Vec::with_capacity(1500);

        match self.udp_socket.try_recv(&mut temp_buf) {
            Ok(len) => {
                let packet = UtpPacket::parse_packet(&temp_buf[..len])?;

                match packet.header.type_ver.packet_type()? {
                    PacketType::Data => {
                        self.handle_data(&packet.header)?;
                        Ok(Some(packet))
                    }
                    PacketType::State => {
                        self.handle_state(&packet.header)?;
                        Ok(None)
                    }
                    PacketType::Syn => {
                        self.handle_syn(&packet.header)?;
                        Ok(None)
                    }
                    PacketType::Fin => {
                        self.handle_fin(&packet.header)?;
                        Ok(None)
                    }
                    PacketType::Reset => {
                        self.handle_reset(&packet.header)?;
                        Ok(None)
                    }
                }
            }
            Err(e) => Err(e),
        }
    }

    async fn send_ack(&mut self) -> io::Result<()> {
        let header = self.utp_header.new_state().to_bytes();
        self.send_packet(&header).await
    }

    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.udp_socket.local_addr()
    }

    /// Creates a new UTP socket bound to the specified address
    pub async fn bind<A: ToSocketAddrs>(addr: A) -> std::io::Result<Self> {
        let udp_socket = Arc::new(UdpSocket::bind(addr).await?);
        let (control_tx, control_rx) = mpsc::channel(32);

        Ok(UtpSocket {
            udp_socket,
            control_tx,
            control_rx,
            buf: BytesMut::new(),
            utp_header: UtpHeader::new(10_000),
            state: ConnectionState::Closed,
            cur_window: 0.into(),
            max_window: 10_000.into(),
            // default to 100ms in microseconds
            rtt: AtomicU64::new(100_000),
            last_packet_time: Instant::now(),
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

        self.state = ConnectionState::SynSent;
        self.udp_socket.connect(remote_addr).await?;
        self.send_syn().await?;

        Ok(())
    }

    /// Sends a SYN packet to initiate connection
    async fn send_syn(&mut self) -> std::io::Result<()> {
        let header = self.utp_header.new_syn();
        let packet = header.to_bytes();
        self.udp_socket.send(&packet).await?;
        Ok(())
    }

    /// Handles SYN packet
    fn handle_syn(&mut self, header: &Header) -> std::io::Result<()> {
        self.state = ConnectionState::SynRecv;
        self.utp_header.handle_recv_syn(header);
        Ok(())
    }

    async fn send_state(&mut self) -> std::io::Result<()> {
        let header = self.utp_header.new_state();
        self.udp_socket.send(&header.to_bytes()).await?;
        Ok(())
    }

    fn handle_state(&mut self, header: &Header) -> std::io::Result<()> {
        self.state = ConnectionState::Connected;
        self.utp_header.handle_recv_state(header);
        Ok(())
    }

    async fn send_data(&mut self) -> std::io::Result<()> {
        let header = self.utp_header.new_data();
        self.udp_socket.send(&header.to_bytes()).await?;
        Ok(())
    }

    fn handle_data(&mut self, header: &Header) -> std::io::Result<()> {
        self.state = ConnectionState::Connected;
        self.utp_header.handle_recv_data(header);
        Ok(())
    }

    /// Send FIN packet to close the connection.
    async fn send_fin(&mut self) -> std::io::Result<()> {
        let header = self.utp_header.new_fin();
        let packet = header.to_bytes();
        self.udp_socket.send(&packet).await?;
        Ok(())
    }

    fn handle_fin(&self, header: &Header) -> std::io::Result<()> {
        todo!()
    }

    fn handle_reset(&self, header: &Header) -> std::io::Result<()> {
        todo!()
    }

    fn cur_window(&self) -> u32 {
        self.cur_window.load(Ordering::Relaxed)
    }

    pub fn remote_addr(&self) -> std::io::Result<SocketAddr> {
        self.udp_socket.peer_addr()
    }

    /// Sends the entire buffer to the socket and cleans it.
    async fn send_packet(&mut self, buf: &[u8]) -> std::io::Result<()> {
        self.udp_socket.send(buf).await?;
        self.last_packet_time = Instant::now();
        Ok(())
    }
}

fn current_timestamp() -> u32 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_micros() as u32
}

#[cfg(test)]
mod tests {
    use futures::{
        SinkExt,
        stream::{SplitSink, StreamExt},
    };
    use tokio::io::AsyncWriteExt;
    use tokio_util::codec::{Decoder, Encoder, Framed};

    use super::*;

    struct TestCodec;
    impl Encoder<u8> for TestCodec {
        type Error = std::io::Error;

        fn encode(
            &mut self,
            item: u8,
            dst: &mut BytesMut,
        ) -> Result<(), Self::Error> {
            dst.put_u8(item);
            Ok(())
        }
    }

    impl Decoder for TestCodec {
        type Error = std::io::Error;
        type Item = u8;

        fn decode(
            &mut self,
            src: &mut BytesMut,
        ) -> Result<Option<Self::Item>, Self::Error> {
            Ok(Some(src.get_u8()))
        }
    }

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

        let mut framed = Framed::new(sender, TestCodec);
        let (mut sink, mut stream) = framed.split();

        tokio::spawn(async move {
            let bytes_sent = sink.send(7).await;
        });

        tokio::spawn(async move {
            let bytes_recv = stream.next().await;
        });
    }
}
