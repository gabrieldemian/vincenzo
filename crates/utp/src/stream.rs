use crate::PacketType;
use std::{
    collections::VecDeque,
    io::{self},
    net::SocketAddr,
    sync::Arc,
    task::Poll,
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::{ToSocketAddrs, UdpSocket, lookup_host},
};
use tokio_util::bytes::{Buf, Bytes, BytesMut};

use crate::{
    CongestionControl, Header, Packet, SentPacket, UtpHeader, current_timestamp,
};

/// Connection state for UTP socket
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub(crate) enum ConnectionState {
    SynSent,
    SynRecv,
    Connected,
    Closing,
    #[default]
    Closed,
}

pub struct UtpStream {
    /// Inner UDP socket.
    pub(crate) socket: Arc<UdpSocket>,

    /// This is a header manager that keeps track of internal data to create
    /// new headers.
    pub(crate) utp_header: UtpHeader,

    pub(crate) state: ConnectionState,
    pub(crate) cc: CongestionControl,

    /// Non-acked received packets.
    pub(crate) incoming_buf: Vec<BytesMut>,

    /// Non-acked sent packets.
    pub(crate) sent_packets: VecDeque<SentPacket>,

    /// Packets not yet sent.
    pub(crate) write_buf: Vec<Packet>,
}

impl UtpStream {
    pub(crate) async fn new(to_bind: impl ToSocketAddrs) -> io::Result<Self> {
        Ok(UtpStream {
            socket: Arc::new(UdpSocket::bind(to_bind).await?),
            state: ConnectionState::Closed,
            utp_header: UtpHeader::default(),
            cc: CongestionControl::default(),
            incoming_buf: Vec::default(),
            write_buf: Vec::default(),
            sent_packets: VecDeque::default(),
        })
    }

    pub(crate) fn from_socket(socket: UdpSocket) -> Self {
        UtpStream {
            socket: Arc::new(socket),
            state: ConnectionState::Closed,
            utp_header: UtpHeader::default(),
            cc: CongestionControl::default(),
            incoming_buf: Vec::default(),
            write_buf: Vec::default(),
            sent_packets: VecDeque::default(),
        }
    }

    pub(crate) fn on_packet_acked(
        &mut self,
        outstanding_bytes: u32,
        timestamp_diff: u32,
    ) {
        self.cc.update_rtt(self.utp_header.get_rtt(), timestamp_diff);
        self.cc.update_window(outstanding_bytes, timestamp_diff);
    }

    /// Initiate the UTP connection by sending a SYN to the remote `addr` but
    /// doesn't wait for an ACK.
    pub async fn connect(addr: impl ToSocketAddrs) -> io::Result<UtpStream> {
        let addr = lookup_host(addr)
            .await?
            .next()
            .ok_or(io::ErrorKind::InvalidInput)?;

        let to_bind = match addr {
            SocketAddr::V4(_) => "0.0.0.0:0",
            SocketAddr::V6(_) => "[::]:0",
        };

        let mut stream = UtpStream::new(to_bind).await?;

        stream.socket.connect(addr).await?;
        stream.send_syn().await?;
        println!("sent sync");

        Ok(stream)
    }

    fn try_receive_packet(
        &mut self,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<io::Result<Packet>> {
        match self.socket.poll_recv(cx, buf) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(r) => match r {
                Ok(()) => {
                    let packet = Packet::from_bytes(buf.initialized())?;

                    self.on_packet_acked(
                        self.sent_packets.iter().fold(0, |acc, v| {
                            acc + v.packet.payload.remaining() as u32
                        }),
                        packet.header.timestamp_diff,
                    );

                    match packet.header.type_ver.packet_type()? {
                        PacketType::Data => {
                            println!("recv data");

                            self.handle_data(&packet);

                            if let Some(state) = self.sent_packets.iter().last()
                            {
                                self.socket
                                    .poll_send(cx, &state.packet.as_bytes_mut())
                                    .map_ok(|_| packet)
                            } else {
                                Poll::Ready(Ok(packet))
                            }
                        }
                        PacketType::State => {
                            println!("recv state");
                            self.handle_state(&packet.header);
                            Poll::Ready(Ok(packet))
                        }
                        PacketType::Syn => {
                            println!("received syn");

                            self.handle_syn(&packet.header);
                            self.send_state();

                            if let Some(state) = self.sent_packets.iter().last()
                            {
                                self.socket
                                    .poll_send(cx, &state.packet.as_bytes_mut())
                                    .map_ok(|_| packet)
                            } else {
                                Poll::Ready(Ok(packet))
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
        self.socket.local_addr()
    }

    /// Sends a SYN packet to initiate connection
    pub(crate) async fn send_syn(&mut self) -> std::io::Result<()> {
        let wnd = self.cc.window();
        let header = self.utp_header.new_syn(wnd);
        let header_bytes = header.as_bytes_mut();

        self.sent_packets.push_back(SentPacket {
            packet: header.into(),
            ..Default::default()
        });

        self.socket.send(&header_bytes).await?;
        self.state = ConnectionState::SynSent;

        Ok(())
    }

    /// Handles SYN packet
    pub(crate) fn handle_syn(&mut self, header: &Header) {
        if self.state == ConnectionState::Closed {
            self.state = ConnectionState::SynRecv;
        }
        self.utp_header.handle_syn(header);
    }

    /// Send ACK.
    fn send_state(&mut self) {
        let wnd = self.cc.window();
        let header = self.utp_header.new_state(wnd);
        self.sent_packets.push_back(SentPacket {
            packet: Packet { header, payload: Bytes::new() },
            ..Default::default()
        });
    }

    /// Handle ACKs
    fn handle_state(&mut self, header: &Header) {
        if self.state == ConnectionState::SynSent {
            self.state = ConnectionState::Connected;
        }
        let ack_nr = header.ack_nr;
        let last_ack = self.utp_header.last_ack_nr();

        // this is a new ACK
        if (ack_nr > last_ack) || (ack_nr == 0 && last_ack == 0) {
            self.utp_header.handle_new_ack(header);
            self.process_new_ack(ack_nr);
        }
        // this is a duplicate ACK
        else {
            let count = self.utp_header.inc_duplicate_ack(header.seq_nr);
            if count >= 3 {
                // triple duplicate ACK - packet loss detected
                self.handle_packet_loss(ack_nr + 1);
            }
        }
    }

    /// Process new ACK and remove acknowledged packets from send buffer
    fn process_new_ack(&mut self, ack_nr: u16) {
        let send_buffer = &mut self.sent_packets;
        let ack_history = &mut self.utp_header.ack_history;

        // remove all packets with seq_nr <= ack_nr
        while let Some(packet) = send_buffer.front() {
            if packet.packet.header.seq_nr <= ack_nr {
                send_buffer.pop_front();
            } else {
                break;
            }
        }

        // clear ACK history for acknowledged packets
        ack_history.retain(|&seq, _| seq > ack_nr);
    }

    fn check_timeouts(&mut self) {
        let timeout = self.cc.timeout();
        let now = current_timestamp();

        for packet in self.sent_packets.iter() {
            if now.saturating_sub(packet.packet.header.timestamp) >= timeout {
                self.handle_packet_loss(packet.packet.header.seq_nr);
                // todo: breaking here to shutup the compiler, find a work
                // around.
                break;
            }
        }
    }

    fn handle_packet_loss(&mut self, seq_nr: u16) {
        self.cc.packet_loss();
        self.retransmit_packet(seq_nr);
        self.utp_header.zero_duplicate_ack(seq_nr);
    }

    /// Retransmit a specific packet
    fn retransmit_packet(&mut self, seq_nr: u16) {
        let send_buffer = &mut self.sent_packets;

        if let Some(packet) =
            send_buffer.iter_mut().find(|p| p.packet.header.seq_nr == seq_nr)
            && packet.retransmit_count < 10
        {
            {
                // limit retransmissions
                packet.retransmit_count += 1;
                // todo: should I update the timestamp on retransmission ?
                packet.packet.header.timestamp = current_timestamp();
                packet.packet.header.wnd_size = self.cc.window();
                self.write_buf.push(packet.packet.clone());
            }
        }
    }

    fn push_to_write_buf(&mut self, payload: &[u8]) {
        let wnd = self.cc.window();
        let header = self.utp_header.new_data(wnd);
        let packet = Packet::new(header, payload);
        self.write_buf.push(packet);
    }

    fn handle_data(&mut self, packet: &Packet) {
        let header = &packet.header;
        self.state = ConnectionState::Connected;
        self.utp_header.handle_data(header);

        self.process_new_ack(header.ack_nr);

        // if there is no data to send, ACK right now, otherwise the next data
        // packet will already ACK.
        if !self.any_packet_to_send() {
            self.send_state();
        }
    }

    /// Send FIN packet to close the connection.
    async fn send_fin(&mut self) -> std::io::Result<()> {
        let wnd = self.cc.window();
        let header = self.utp_header.new_fin(wnd);
        let packet = header.to_bytes();
        self.socket.send(&packet).await?;
        Ok(())
    }

    fn any_packet_to_send(&self) -> bool {
        !self.write_buf.is_empty()
            && self.write_buf.iter().any(|v| !v.payload.is_empty())
    }

    fn handle_fin(&self, header: &Header) -> std::io::Result<()> {
        todo!()
    }

    fn handle_reset(&self, header: &Header) -> std::io::Result<()> {
        todo!()
    }

    pub fn remote_addr(&self) -> std::io::Result<SocketAddr> {
        self.socket.peer_addr()
    }
}

impl AsyncRead for UtpStream {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        for incoming_buf in self.incoming_buf.iter_mut() {
            let copy_len = incoming_buf.remaining().min(buf.remaining());

            // copy data from our buffer to the user's buffer
            buf.put_slice(&incoming_buf[..copy_len]);
            incoming_buf.advance(copy_len);

            if buf.remaining() == 0 {
                return Poll::Ready(Ok(()));
            }
        }

        match self.try_receive_packet(cx, buf) {
            Poll::Pending => {
                // cx.waker().wake_by_ref();
                self.socket.poll_recv_ready(cx)
            }
            Poll::Ready(v) => match v {
                Ok(_packet) => Poll::Ready(Ok(())),
                Err(e) => Poll::Ready(Err(e)),
            },
        }
    }
}

impl AsyncWrite for UtpStream {
    /// Every cal to `poll_write` is to write a full packet. Each packet will be
    /// sent alone to the peer.
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, std::io::Error>> {
        // ensure we have enough capacity in the buffer
        // if self.sent_packets.iter().fold(0, |acc, v| acc + v.data.len())
        //     < buf.len()
        // {
        // try to flush and then check again
        // match self.as_mut().poll_flush(cx) {
        //     Poll::Ready(Ok(())) => {
        //         if self
        //             .sent_packets
        //             .iter()
        //             .fold(0, |acc, v| acc + v.data.len())
        //             < buf.len()
        //         {
        //             // still not enough space, apply backpressure
        //             cx.waker().wake_by_ref();
        //             return Poll::Pending;
        //         }
        //     }
        //     Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
        //     Poll::Pending => {
        //         cx.waker().wake_by_ref();
        //         return Poll::Pending;
        //     }
        // }
        // }
        self.push_to_write_buf(buf);
        self.check_timeouts();
        Poll::Ready(Ok(buf.len()))
    }

    /// Gather the bytes in the buffer and send a UTP packet of type data.
    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        self.check_timeouts();

        if self.write_buf.is_empty() {
            return Poll::Ready(Ok(()));
        }

        let mut r = Poll::Ready(Ok(()));
        let mut to_sent = Vec::new();
        let mut to_delete = vec![true; self.write_buf.len()];

        for (i, packet) in self.write_buf.iter().enumerate() {
            let packet_bytes = packet.as_bytes_mut();
            let packet_len = packet_bytes.len();

            match self.socket.poll_send(cx, &packet_bytes) {
                Poll::Ready(Ok(n)) => {
                    if n == packet_len {
                        to_sent.push(SentPacket {
                            packet: packet.clone(),
                            ..Default::default()
                        });
                        to_delete[i] = false;
                    } else {
                        // r = Poll::Pending;
                        // self.socket.poll_send_ready(cx);
                        // break;
                        // Poll::Ready(Err(io::Error::other(
                        //     "Partial UDP packet send occurred",
                        // )))
                    }
                }
                Poll::Ready(Err(e)) => {
                    r = Poll::Ready(Err(e));
                    break;
                }
                Poll::Pending => {
                    cx.waker().wake_by_ref();
                    // self.socket.poll_send_ready(cx);
                    // Poll::Pending
                }
            };
        }

        self.sent_packets.extend(to_sent);
        let mut iter = to_delete.into_iter();
        self.write_buf.retain(|_| iter.next().unwrap());

        r
    }

    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        println!("shutting down");
        self.state = ConnectionState::Closing;

        match self.as_mut().poll_flush(cx) {
            Poll::Ready(Ok(_)) => match self.socket.poll_send(cx, &[8]) {
                Poll::Pending => {
                    cx.waker().wake_by_ref();
                    Poll::Pending
                }
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
            Poll::Pending => {
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        }
    }
}
