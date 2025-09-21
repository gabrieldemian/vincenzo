//! Micro Transport Protocol (μTP)

mod header;
use std::time::{SystemTime, UNIX_EPOCH};

use hashbrown::HashMap;
pub(crate) use header::*;

mod packet;
pub(crate) use packet::*;

mod listener;
pub use listener::*;

mod stream;
pub use stream::*;

mod congestion_control;
pub(crate) use congestion_control::*;

use tokio_util::bytes::{BufMut, Bytes, BytesMut};

fn current_timestamp() -> u32 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_micros() as u32
}

#[cfg(test)]
mod tests {
    use std::io;
    use tokio_util::bytes::Buf;

    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::UdpSocket,
    };
    use tokio_util::codec::{Decoder, Encoder};

    use super::*;

    /// Test that the example provided in the BEP 0029 works.
    /// https://www.bittorrent.org/beps/bep_0029.html
    #[tokio::test]
    async fn bep_29_example() -> io::Result<()> {
        let sender_socket = UdpSocket::bind("0.0.0.0:0").await?;
        let receiver_socket = UdpSocket::bind("0.0.0.0:0").await?;

        sender_socket.connect(receiver_socket.local_addr().unwrap()).await?;
        receiver_socket.connect(sender_socket.local_addr().unwrap()).await?;

        let mut receiver = UtpStream::from_socket(receiver_socket);
        let mut sender = UtpStream::from_socket(sender_socket);

        //
        // sender -- syn --> receiver
        //
        sender.send_syn().await?;
        assert_eq!(sender.state, ConnectionState::SynSent);

        let mut buf = [0u8; 20];
        receiver.read_exact(&mut buf).await.unwrap();
        let syn = Packet::from_bytes(&buf)?;
        println!("syn {syn:#?}");

        assert_eq!(sender.sent_packets.len(), 1);
        assert_eq!(syn.header.type_ver, TypeVer::from_packet(PacketType::Syn));
        assert_eq!(syn.header.seq_nr, 0);
        assert_eq!(syn.header.ack_nr, 0);
        assert!(
            syn.header.conn_id > 0,
            "on syn, the sender put his recv_conn_id on conn_id."
        );
        assert_eq!(syn.header.timestamp_diff, 0);
        assert!(syn.header.timestamp > 0);
        assert_eq!(receiver.state, ConnectionState::SynRecv);

        //
        // sender <- ack -- receiver
        //
        let mut buf = [0u8; 20];
        sender.read_exact(&mut buf).await?;
        let ack = Packet::from_bytes(&buf)?;
        println!("ack {ack:#?}");

        assert_eq!(sender.state, ConnectionState::Connected);
        assert_eq!(
            ack.header.type_ver,
            TypeVer::from_packet(PacketType::State)
        );
        assert_eq!(
            ack.header.conn_id, syn.header.conn_id,
            "the send_conn_id of the receiver is the recv_conn_id of the \
             sender."
        );
        assert!(ack.header.timestamp > 0);
        assert!(ack.header.timestamp_diff > 0);
        assert!(ack.header.seq_nr > 0);
        assert_eq!(ack.header.ack_nr, syn.header.seq_nr);

        //
        // sender -- data --> receiver
        //
        sender.write_u8(1).await?;
        sender.flush().await?;

        let mut buf = [0u8; 21];
        receiver.read_exact(&mut buf).await?;
        let data = Packet::from_bytes(&buf)?;
        println!("data {data:#?}");

        assert_eq!(
            data.header.type_ver,
            TypeVer::from_packet(PacketType::Data)
        );
        assert!(
            !receiver.sent_packets.is_empty(),
            "receiver has an ack to send"
        );
        assert_eq!(sender.sent_packets.len(), 1);
        assert_eq!(sender.sent_packets[0].packet, data);
        assert_eq!(receiver.state, ConnectionState::Connected);
        assert_eq!(
            data.header.conn_id,
            syn.header.conn_id + 1,
            "after syn, the sender will put his send_conn_id on conn_id"
        );
        assert_eq!(data.header.ack_nr, ack.header.seq_nr);
        assert_eq!(data.header.seq_nr, syn.header.seq_nr + 1);
        assert_eq!(*data.payload, [1]);

        // here, when the receiver gets a data packet there are 2 options:
        // - receiver has data to send, send data packet acking the previous
        //   received data.
        // - receiver doesn't have data, send ack packet.

        //
        // sender <- ack2 -- receiver
        //
        receiver.flush().await?;
        let mut buf = [0u8; 20];
        sender.read_exact(&mut buf).await?;
        let ack2 = Packet::from_bytes(&buf)?;
        println!("ack2 {ack2:#?}");

        assert_eq!(
            ack2.header.type_ver,
            TypeVer::from_packet(PacketType::State)
        );
        assert_eq!(
            receiver.sent_packets.len(),
            1,
            "receiver has 1 unacked ack"
        );
        assert!(
            sender.sent_packets.is_empty(),
            "receiver acked every packet from sender"
        );
        assert!(sender.incoming_buf.is_empty());
        assert_eq!(ack2.header.conn_id, ack.header.conn_id);
        assert_eq!(ack2.header.seq_nr - 1, ack.header.seq_nr);
        assert_eq!(
            ack2.header.ack_nr, data.header.seq_nr,
            "ack2 should ack the previous data"
        );

        //
        // sender <- data2 -- receiver
        //
        receiver.write_u8(2).await?;
        receiver.flush().await?;
        let mut buf = [0u8; 21];
        sender.read_exact(&mut buf).await?;
        let data2 = Packet::from_bytes(&buf)?;
        println!("data2 {data2:#?}");
        assert_eq!(
            // ack 33
            data2.header.type_ver,
            TypeVer::from_packet(PacketType::Data)
        );
        assert_eq!(data2.header.conn_id, syn.header.conn_id);
        assert_eq!(
            data2.header.ack_nr, data.header.seq_nr,
            "data2 should ack the previous data"
        );
        assert_eq!(data2.header.seq_nr - 1, ack2.header.seq_nr);
        assert_eq!(*data2.payload, [2]);

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
