//! Micro Transport Protocol (μTP)

mod header;
pub(crate) use header::*;

mod packet;
pub(crate) use packet::*;

use std::{
    future::poll_fn,
    io::{self},
    net::SocketAddr,
    ops::Deref,
    pin::pin,
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
}

impl AsyncWrite for UtpSocket {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, std::io::Error>> {
        // println!("writing buf {buf:?}",);

        // ensure we have enough capacity in the buffer
        if self.buf.remaining_mut() < buf.len() {
            // If we don't have enough space, we could:
            // 1. Try to flush existing data
            // 2. Allocate more space
            // 3. Return WouldBlock to apply backpressure

            // return Poll::Ready(Err(io::ErrorKind::WouldBlock));

            // try to flush and then check again
            match self.as_mut().poll_flush(cx) {
                Poll::Ready(Ok(())) => {
                    if self.buf.remaining_mut() < buf.len() {
                        // still not enough space, apply backpressure
                        cx.waker().wake_by_ref();
                        return Poll::Pending;
                    }
                }
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => {
                    // we need to wait for flush to complete
                    cx.waker().wake_by_ref();
                    return Poll::Pending;
                }
            }
        }
        self.buf.extend_from_slice(buf);
        Poll::Ready(Ok(buf.len()))
    }

    /// Gather the bytes in the buffer and send a UTP packet of type data.
    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        while let Poll::Ready(Some(mut payload)) = self.control_rx.poll_recv(cx)
        {
            match self.udp_socket.poll_send(cx, &payload) {
                Poll::Ready(Ok(n)) => {
                    if n == payload.len() {
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
                if n == packet.len() {
                    Poll::Ready(Ok(()))
                } else {
                    cx.waker().wake_by_ref();
                    Poll::Ready(Err(io::Error::other(
                        "Partial UDP packet send occurred",
                    )))
                }
            }
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Pending => Poll::Pending,
        }
    }

    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
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
    ) -> Poll<std::io::Result<()>> {
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

        match self.try_receive_packet(cx, buf) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(v) => match v {
                Ok(_packet) => Poll::Ready(Ok(())),
                Err(e) => Poll::Ready(Err(e)),
            },
        }
    }
}

impl UtpSocket {
    fn try_receive_packet(
        &mut self,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<io::Result<UtpPacket>> {
        match self.udp_socket.poll_recv(cx, buf) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(r) => match r {
                Ok(()) => {
                    let packet = UtpPacket::from_bytes(buf.initialized())?;
                    match packet.header.type_ver.packet_type()? {
                        PacketType::Data => {
                            println!("recv data");
                            self.handle_data(&packet.header)?;
                            Poll::Ready(Ok(packet))
                        }
                        PacketType::State => {
                            println!("recv state");
                            self.handle_state(&packet.header)?;
                            Poll::Ready(Ok(packet))
                        }
                        PacketType::Syn => {
                            let sender = self.udp_socket.poll_peek_sender(cx);

                            match sender {
                                Poll::Pending => Poll::Pending,
                                Poll::Ready(v) => match v {
                                    Ok(sender) => {
                                        println!("recv syn sender: {sender:?}",);

                                        self.handle_syn(&packet.header)?;

                                        let header = self
                                            .utp_header
                                            .new_state()
                                            .to_bytes();

                                        self.udp_socket
                                            .poll_send_to(cx, &header, sender)
                                            .map_ok(|_| packet)
                                    }
                                    Err(e) => Poll::Ready(Err(e)),
                                },
                            }
                        }
                        PacketType::Fin => {
                            self.handle_fin(&packet.header)?;
                            Poll::Ready(Ok(packet))
                        }
                        PacketType::Reset => {
                            self.handle_reset(&packet.header)?;
                            Poll::Ready(Ok(packet))
                        }
                    }
                }
                Err(e) => Poll::Ready(Err(e)),
            },
        }
    }

    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.udp_socket.local_addr()
    }

    /// Creates a new UTP socket bound to the specified address
    pub async fn bind<A: ToSocketAddrs>(addr: A) -> std::io::Result<Self> {
        let udp_socket = Arc::new(UdpSocket::bind(addr).await?);
        let (control_tx, control_rx) = mpsc::channel(10_000);

        Ok(UtpSocket {
            udp_socket,
            control_tx,
            control_rx,
            buf: BytesMut::with_capacity(10_000),
            utp_header: UtpHeader::new(10_000),
            state: ConnectionState::Closed,
            cur_window: 10_000.into(),
            max_window: 10_000.into(),

            // default to 100ms in microseconds
            rtt: AtomicU64::new(100_000),
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
        self.state = ConnectionState::SynSent;
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
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio_util::codec::{Decoder, Encoder, Framed};

    use super::*;

    /// Test that the example provided in the BEP 0029 works.
    /// https://www.bittorrent.org/beps/bep_0029.html
    #[tokio::test]
    async fn bep_29_example() -> io::Result<()> {
        let mut sender = UtpSocket::bind("0.0.0.0:34254")
            .await
            .expect("Failed to bind UTP socket");

        let mut receiver = UtpSocket::bind("0.0.0.0:34255")
            .await
            .expect("Failed to bind UDP socket");

        sender
            .connect(receiver.local_addr().unwrap())
            .await
            .expect("Failed to connect");

        assert_eq!(sender.state, ConnectionState::SynSent);

        sender.write_u8(1).await?;
        sender.write_u8(2).await?;
        sender.write_u8(3).await?;
        sender.flush().await?;

        let mut buf = [0u8; 20];
        receiver.read_exact(&mut buf).await?;
        let syn = UtpPacket::from_bytes(&buf)?;
        println!("syn {syn:#?}");

        assert_eq!(receiver.state, ConnectionState::SynRecv);

        let mut buf = [0u8; 20];
        sender.read_exact(&mut buf).await?;
        let state = UtpPacket::from_bytes(&buf)?;
        println!("state {state:#?}");

        assert_eq!(sender.state, ConnectionState::Connected);

        sender.write_u8(1).await?;
        sender.flush().await?;

        let mut buf = [0u8; 21];
        receiver.read_exact(&mut buf).await?;
        let data = UtpPacket::from_bytes(&buf)?;
        println!("data {data:#?}");

        assert_eq!(receiver.state, ConnectionState::Connected);

        Ok(())
    }

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
}
