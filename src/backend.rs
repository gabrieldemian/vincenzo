use crate::frontend::FrontendMessage;
use crate::magnet_parser::get_info_hash;
use crate::magnet_parser::get_magnet;
use crate::tracker::client::Client;
use actix::prelude::*;

#[derive(Message)]
#[rtype(result = "()")]
pub enum BackendMessage {
    Quit,
    AddTorrent,
    DeleteTorrent,
}

pub struct Backend {
    recipient: Recipient<FrontendMessage>,
}

impl Actor for Backend {
    type Context = Context<Self>;

    fn started(&mut self, _ctx: &mut Self::Context) {
        // let args = Args::parse();
        //
        // if let Ok(m) = magnet {
        // }
    }

    fn stopping(&mut self, _ctx: &mut Self::Context) -> Running {
        Running::Stop
    }
}

impl Backend {
    pub fn new(recipient: Recipient<FrontendMessage>) -> Self {
        Self { recipient }
    }
}

impl Handler<BackendMessage> for Backend {
    type Result = ();

    fn handle(&mut self, msg: BackendMessage, ctx: &mut Self::Context) -> Self::Result {
        match msg {
            BackendMessage::Quit => {
                ctx.stop();
                System::current().stop();
            }
            _ => {}
        }
    }
}

#[cfg(test)]
pub mod tests {

    use std::{io::Read, time::Duration};

    use actix::clock::timeout;
    use actix_rt::net::{TcpListener, TcpStream, UdpSocket};
    use speedy::{BigEndian, Readable, Writable};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use crate::error::Error;

    use super::*;

    #[tokio::test]
    async fn udp() -> Result<(), std::io::Error> {
        // let magnet = "magnet:?xt=urn:btih:48AAC768A865798307DDD4284BE77644368DD2C7&amp;dn=Kerkour%20S.%20Black%20Hat%20Rust...Rust%20programming%20language%202022&amp;tr=udp%3A%2F%2Ftracker.coppersurfer.tk%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.openbittorrent.com%3A6969%2Fannounce&amp;tr=udp%3A%2F%2F9.rarbg.to%3A2710%2Fannounce&amp;tr=udp%3A%2F%2F9.rarbg.me%3A2780%2Fannounce&amp;tr=udp%3A%2F%2F9.rarbg.to%3A2730%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.opentrackr.org%3A1337&amp;tr=http%3A%2F%2Fp4p.arenabg.com%3A1337%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.torrent.eu.org%3A451%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.tiny-vps.com%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Fopen.stealth.si%3A80%2Fannounce";
        let magnet = "magnet:?xt=urn:btih:56BC861F42972DEA863AE853362A20E15C7BA07E&dn=Rust%20for%20Rustaceans%3A%20Idiomatic%20Programming&tr=udp%3A%2F%2Ftracker.coppersurfer.tk%3A6969%2Fannounce&tr=udp%3A%2F%2Ftracker.openbittorrent.com%3A6969%2Fannounce&tr=udp%3A%2F%2F9.rarbg.to%3A2710%2Fannounce&tr=udp%3A%2F%2F9.rarbg.me%3A2780%2Fannounce&tr=udp%3A%2F%2F9.rarbg.to%3A2730%2Fannounce&tr=udp%3A%2F%2Ftracker.opentrackr.org%3A1337&tr=http%3A%2F%2Fp4p.arenabg.com%3A1337%2Fannounce&tr=udp%3A%2F%2Ftracker.torrent.eu.org%3A451%2Fannounce&tr=udp%3A%2F%2Ftracker.tiny-vps.com%3A6969%2Fannounce&tr=udp%3A%2F%2Fopen.stealth.si%3A80%2Fannounce";

        let magnet = get_magnet(magnet).unwrap();

        // println!("magnet {:#?}", magnet);
        //
        let client = Client::connect(magnet.tr).unwrap();
        //
        println!("client {:#?}", client);
        //
        let info_hash = get_info_hash(&magnet.xt.unwrap());

        let peers = client.announce_exchange(info_hash).unwrap();

        #[derive(Clone, Debug, Writable, Readable)]
        struct Handshake {
            pub pstr_len: u8,
            pub pstr: [u8; 19],
            pub reserved: [u8; 8],
            pub info_hash: [u8; 20],
            pub peer_id: [u8; 20],
        }

        impl Handshake {
            fn new(info_hash: [u8; 20], peer_id: [u8; 20]) -> Self {
                Self {
                    pstr_len: u8::to_be(19),
                    pstr: b"BitTorrent protocol".to_owned(),
                    reserved: [0u8; 8],
                    info_hash,
                    peer_id,
                }
            }
            fn serialize(&self) -> Result<Vec<u8>, Error> {
                self.write_to_vec_with_ctx(BigEndian {})
                    .map_err(Error::SpeedyError)
            }
            fn deserialize(buf: &[u8]) -> Self {
                Self::read_from_buffer_with_ctx(BigEndian {}, buf).unwrap()
            }
            fn validate(&self, target: Self) -> bool {
                if target.peer_id.len() != 20 {
                    eprintln!("-- warning -- invalid peer_id from receiving handshake");
                    return false;
                }
                if self.info_hash != self.info_hash {
                    eprintln!(
                        "-- warning -- info_hash from receiving handshake does not match ours"
                    );
                    return false;
                }
                if target.pstr_len != 19 {
                    eprintln!("-- warning -- handshake with wrong pstr_len, dropping connection");
                    return false;
                }
                if target.pstr != b"BitTorrent protocol".to_owned() {
                    eprintln!("-- warning -- handshake with wrong pstr, dropping connection");
                    return false;
                }
                true
            }
        }

        // let info_hash = [2u8; 20];
        // let peer_id = [1u8; 20];

        let handshake = Handshake::new(info_hash, client.peer_id);

        let a = "213.227.151.222:28421";
        // let a = "197.184.177.151:6881";
        // let a = "187.198.250.155:6881";
        // let a = "186.22.54.178:27730";
        // let a = "174.130.112.93:64153";
        // let a = "146.70.198.56:51413";
        // let a = "108.172.55.95:37874";
        // let a = "85.8.130.72:37874";
        // let a = "46.126.71.42:51413";

        // send handshake to peer
        let mut socket = timeout(
            Duration::from_secs(5),
            // UdpSocket::bind(a)
            TcpStream::connect(a),
        )
        .await??;

        // if the TcpStream is Some, that means the peer
        // is on the peer_wire protocol,
        // if not, and UdpSocket is Some, that means the peer
        // is on uTP protocol. I need to do an if-else
        // to call each protocol for that peer

        println!("tcp socket? {:#?}", socket);

        // client -> handshake -> peer
        // client <- handshake <- peer
        // client -> interested -> peer
        // client <- unchoke <- peer
        // client <- have|bitfield <- peer
        // client -> request -> peer
        //
        // <length prefix><message ID><payload>

        let (mut rd, mut wr) = socket.split();

        // send handshake
        println!("sending handshake...");
        wr.write_all(&mut handshake.serialize().unwrap()).await?;
        println!("done");

        let mut buf: Vec<u8> = vec![];

        // receive handshake
        println!("receiving handshake...");
        let rd_len = rd.read_to_end(&mut buf).await?;

        println!("my peer_id {:?}", client.peer_id);
        println!("my info hash {:?}", info_hash);
        println!("with len {rd_len}");

        let handshake_remote = Handshake::deserialize(&buf);
        println!("deserialized {:?}", handshake_remote);

        handshake_remote.validate(handshake);

        // after the double handshake, this is the only time
        // where a peer is allowed to send his pieces to the client
        // after that, the main even loop will start

        Ok(())
    }
}
