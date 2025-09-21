use crate::*;
use std::{io, sync::Arc};
use tokio::net::{ToSocketAddrs, UdpSocket};

#[derive(Debug)]
pub struct UtpListener {
    socket: Arc<UdpSocket>,
}

impl UtpListener {
    /// Creates a new UTP socket bound to the specified address
    pub async fn bind<A: ToSocketAddrs>(addr: A) -> io::Result<Self> {
        let socket = Arc::new(UdpSocket::bind(addr).await?);
        Ok(UtpListener { socket })
    }

    /// Accepts a new incoming connection from this listener.
    ///
    /// This function will yield once a new TCP connection is established. When
    /// established, the corresponding [`UtpStream`] and the remote peer's
    /// address will be returned.
    pub async fn accept(&self) -> io::Result<UtpStream> {
        let mut buf = [0u8; HEADER_LEN];
        let (_read, sender) = self.socket.recv_from(&mut buf).await?;

        let mut stream = {
            UtpStream {
                sent_packets: Default::default(),
                socket: self.socket.clone(),
                incoming_buf: Vec::new(),
                write_buf: Vec::default(),
                utp_header: UtpHeader::new(),
                state: ConnectionState::Closed,
                cc: CongestionControl::new(),
            }
        };

        stream.socket.connect(sender).await?;
        stream.handle_syn(&Header::from_bytes(&buf)?);

        Ok(stream)
    }
}
