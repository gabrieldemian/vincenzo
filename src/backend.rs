use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use crate::error::Error;
use crate::frontend::FrontendMessage;
use crate::magnet_parser::get_info_hash;
use crate::magnet_parser::get_magnet;
use crate::tcp_wire::messages::Handshake;
use crate::tracker::client::Client;
use actix::clock::timeout;
use actix::prelude::*;
use actix::spawn;
use log::debug;
use log::info;
use log::warn;
use magnet_url::Magnet;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

#[derive(Message)]
#[rtype(result = "()")]
pub enum BackendMessage {
    Quit,
    AddMagnet(String),
    Handshaked(TcpStream),
}

pub struct Backend {
    peers: Arc<Mutex<Vec<TcpStream>>>,
    recipient: Recipient<FrontendMessage>,
}

impl Actor for Backend {
    type Context = Context<Self>;

    fn started(&mut self, ctx: &mut Self::Context) {
        self.run(ctx).unwrap();
    }

    fn stopping(&mut self, _ctx: &mut Self::Context) -> Running {
        Running::Stop
    }
}

impl Backend {
    pub fn new(recipient: Recipient<FrontendMessage>) -> Self {
        let peers = Arc::new(Mutex::new(vec![]));
        Self { recipient, peers }
    }

    fn run(&mut self, ctx: &mut Context<Backend>) -> Result<(), std::io::Error> {
        let tick_rate = Duration::from_millis(100);

        ctx.run_interval(tick_rate, move |act, ctx| {
            // act.recipient.do_send(FrontendMessage::Quit);
        });

        Ok(())
    }

    fn add_magnet(&mut self, ctx: &mut Context<Backend>, m: Magnet) -> Result<(), Error> {
        info!("received add_magnet call");
        info!(
            "magnet with name of {:?}",
            urlencoding::decode(&m.dn.unwrap())
        );

        let info_hash = get_info_hash(&m.xt.unwrap());
        debug!("info_hash {:?}", info_hash);

        // first, do a `connect` handshake to the tracker
        let client = Client::connect(m.tr).unwrap();

        // second, do a `announce` handshake to the tracker
        // and get the list of peers for this torrent
        let peers = client.announce_exchange(info_hash).unwrap();

        // let mut gg = Arc::new(Mutex::new(vec![]));

        spawn(async move {
            for peer in peers {
                info!("trying to connect to {:?}", peer);
                let handshake = Handshake::new(info_hash, client.peer_id);

                let mut socket = match timeout(Duration::from_secs(2), TcpStream::connect(peer))
                    .await
                {
                    Ok(x) => match x {
                        Ok(x) => x,
                        Err(_) => {
                            info!("peer connection refused, skipping");
                            continue;
                        }
                    },
                    Err(_) => {
                        debug!("peer does download_dirnot support peer wire protocol, skipping");
                        continue;
                    }
                };

                info!("connected to socket {:#?}", socket);
                info!("* sending handshake...");

                // send our handshake to the peer
                socket
                    .write_all(&mut handshake.serialize().unwrap())
                    .await?;

                // receive a handshake back
                let mut buf = Vec::with_capacity(100);

                match timeout(Duration::from_secs(25), socket.read_to_end(&mut buf)).await {
                    Ok(x) => {
                        let x = x.unwrap();

                        info!("* received handshake back");

                        if x == 0 {
                            continue;
                        };

                        info!("len {}", x);
                        info!("trying to deserialize it now");

                        let remote_handshake = Handshake::deserialize(&mut buf)?;

                        info!("{:?}", remote_handshake);

                        if remote_handshake.validate(handshake.clone()) {
                            info!("handshake is valid, listening to more messages");
                            // let mut gg = gg.lock().unwrap();
                            // gg.push(socket);
                        }
                    }
                    Err(_) => {
                        warn!("connection timeout");
                    }
                }
            }
            Ok::<(), Error>(())
        });

        Ok(())
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
            BackendMessage::AddMagnet(link) => {
                let m = get_magnet(&link).unwrap();
                self.add_magnet(ctx, m).unwrap();
            }
            _ => {}
        }
    }
}

// #[cfg(test)]
// pub mod tests {
//
//     use super::*;
//
//     #[tokio::test]
//     async fn udp() -> Result<(), Box<dyn std::error::Error>> {
//     }
// }
