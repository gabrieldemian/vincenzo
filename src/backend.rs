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

    use actix::{clock::timeout, spawn};
    use actix_rt::net::{TcpListener, TcpStream, UdpSocket};
    use speedy::{BigEndian, Readable, Writable};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use crate::{
        error::Error,
        tcp_wire::messages::{Handshake, Interested, Unchoke},
    };

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
        // println!("client {:#?}", client);
        //
        let info_hash = get_info_hash(&magnet.xt.unwrap());

        let peers = client.announce_exchange(info_hash).unwrap();

        // let ignore_me = [
        //     19, 66, 105, 116, 84, 111, 114, 114, 101, 110, 116, 32, 112, 114, 111, 116, 111, 99,
        //     111, 108, 0, 0, 0, 0, 0, 16, 0, 5, 86, 188, 134, 31, 66, 151, 45, 234, 134, 58, 232,
        //     83, 54, 42, 32, 225, 92, 123, 160, 126, 45, 84, 82, 50, 57, 52, 48, 45, 102, 103, 109,
        //     57, 104, 57, 116, 122, 119, 110, 116, 103, 0, 0, 0, 36, 5, 255, 255, 255, 255, 255,
        //     255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
        //     255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 192, 0, 0, 0, 1, 1,
        // ];

        // let what = Handshake::deserialize(&ignore_me);
        // println!("what did i receive {:?}", what);

        let a = "192.168.1.5:43506";

        for peer in peers {
            let handshake = Handshake::new(info_hash, client.peer_id);

            // connect with peer
            println!("trying handshake with {:?}", peer);
            let mut socket = match timeout(Duration::from_secs(2), TcpStream::connect(peer)).await {
                Ok(x) => match x {
                    Ok(x) => x,
                    Err(_) => {
                        println!("peer connection refused");
                        continue;
                    }
                },
                Err(_) => {
                    println!("peer does not support peer wire protocol");
                    continue;
                }
            };

            println!("connected to socket {:#?}", socket);

            // if the TcpStream is Some, that means the peer
            // is on the peer_wire protocol,
            // if not, and UdpSocket is Some, that means the peer
            // is on uTP protocol. I need to do an if-else
            // to call each protocol for that peer
            //
            // client -> handshake -> peer
            // client <- handshake <- peer
            // client -> interested -> peer
            // client <- unchoke <- peer
            // client <- have|bitfield <- peer
            // client -> request -> peer
            //
            // <length prefix><message ID><payload>
            //
            // send handshake
            println!("sending handshake...");

            let mut buf: Vec<u8> = vec![];

            // send our handshake to the peer
            socket
                .write_all(&mut handshake.serialize().unwrap())
                .await?;

            println!("done");

            // receive handshake from the peer
            println!("receiving handshake...");
            match timeout(Duration::from_secs(30), socket.read_to_end(&mut buf)).await {
                Ok(a) => a,
                Err(_) => {
                    println!("peer took to long to send handshake");
                    continue;
                }
            }?;

            let handshake_remote = Handshake::deserialize(&buf);
            println!("received {:?}", handshake_remote);

            println!("sending interested msg");
            socket
                .write_all(&Interested::new().serialize().unwrap())
                .await?;


            // send interested message
            // let mut buf: Vec<u8> = vec![];
            // let interested = Interested::new();

            // println!("sending interested message...");
            // socket
            //     .write_all(&mut interested.serialize().unwrap())
            //     .await?;
            // println!("done");

            println!("reading more messages...");
            let mut buf: Vec<u8> = vec![];
            let b = socket.read_to_end(&mut buf).await?;

            println!("reading more messages...");
            let mut buf: Vec<u8> = vec![];
            let b = socket.read_to_end(&mut buf).await?;

            if b > 0 {
                println!("more msg {:?}", buf);
                break;
            } else {
                println!("could not read anything");
                continue;
            }
        }

        Ok(())

        // after the double handshake, this is the only time
        // where a peer is allowed to send his pieces to the client
        // after that, the main even loop will start
    }
}
