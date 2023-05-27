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
    use tokio::io::AsyncWriteExt;

    use super::*;

    #[tokio::test]
    async fn udp() -> Result<(), std::io::Error> {
        let magnet = "magnet:?xt=urn:btih:56BC861F42972DEA863AE853362A20E15C7BA07E&dn=Rust%20for%20Rustaceans%3A%20Idiomatic%20Programming&tr=udp%3A%2F%2Ftracker.coppersurfer.tk%3A6969%2Fannounce&tr=udp%3A%2F%2Ftracker.openbittorrent.com%3A6969%2Fannounce&tr=udp%3A%2F%2F9.rarbg.to%3A2710%2Fannounce&tr=udp%3A%2F%2F9.rarbg.me%3A2780%2Fannounce&tr=udp%3A%2F%2F9.rarbg.to%3A2730%2Fannounce&tr=udp%3A%2F%2Ftracker.opentrackr.org%3A1337&tr=http%3A%2F%2Fp4p.arenabg.com%3A1337%2Fannounce&tr=udp%3A%2F%2Ftracker.torrent.eu.org%3A451%2Fannounce&tr=udp%3A%2F%2Ftracker.tiny-vps.com%3A6969%2Fannounce&tr=udp%3A%2F%2Fopen.stealth.si%3A80%2Fannounce";
        let magnet = get_magnet(magnet).unwrap();

        // println!("magnet {:#?}", magnet);
        //
        // let client = Client::connect(magnet.tr).unwrap();
        //
        // println!("client {:#?}", client);
        //
        // let info_hash = get_info_hash(&magnet.xt.unwrap());

        // let peers = client.announce_exchange(info_hash).unwrap();

        // handshake with peers
        // let peers_tcp = TcpStream::connect(peers[0]).await?;

        let mut handshake = Vec::new(); // 68 bytes

        let info_hash = [2u8; 20];
        let peer_id = [1u8; 20];

        // pstrlen - len of pstrl
        handshake.push(u8::to_be(19));

        // pstrl - identifier of the protocol
        let pstr = b"Bittorrent protocol";
        handshake.extend_from_slice(pstr);

        // reserved 8 bytes
        (0..8).for_each(|_| handshake.push(u8::to_be(0)));

        // info_hash
        handshake.extend_from_slice(&info_hash);

        // peer_id
        handshake.extend_from_slice(&peer_id);

        // let a = "187.7.65.153:44099";
        let a = "187.7.65.153:47994";

        // send handshake to peer
        let mut peer = 
            timeout(
                Duration::from_secs(5),
                UdpSocket::bind(a)
                // TcpStream::connect(a)
            )
            .await?;
        // peer.write_all(&mut handshake.as_slice()).await?;

        Ok(())
    }
}
