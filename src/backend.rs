use crate::frontend::FrontendMessage;
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
    use magnet_url::Magnet;
    use sha1::{Digest, Sha1};
    use url::Url;

    use super::*;
    use crate::tracker::client::Client;
    // http://bt1.archive.org:6969/announce?info_hash=%ac%c3%b2%e43%d7%c7GZ%bbYA%b5h%1c%b7%a1%ea%26%e2&peer_id=ABCDEFGHIJKLMNOPQRST&ip=80.11.255.166&port=6881&downloaded=0&left=970

    #[test]
    fn udp() {
        let magnet = "magnet:?xt=urn:btih:56BC861F42972DEA863AE853362A20E15C7BA07E&dn=Rust%20for%20Rustaceans%3A%20Idiomatic%20Programming&tr=udp%3A%2F%2Ftracker.coppersurfer.tk%3A6969%2Fannounce&tr=udp%3A%2F%2Ftracker.openbittorrent.com%3A6969%2Fannounce&tr=udp%3A%2F%2F9.rarbg.to%3A2710%2Fannounce&tr=udp%3A%2F%2F9.rarbg.me%3A2780%2Fannounce&tr=udp%3A%2F%2F9.rarbg.to%3A2730%2Fannounce&tr=udp%3A%2F%2Ftracker.opentrackr.org%3A1337&tr=http%3A%2F%2Fp4p.arenabg.com%3A1337%2Fannounce&tr=udp%3A%2F%2Ftracker.torrent.eu.org%3A451%2Fannounce&tr=udp%3A%2F%2Ftracker.tiny-vps.com%3A6969%2Fannounce&tr=udp%3A%2F%2Fopen.stealth.si%3A80%2Fannounce";
        let magnet = Magnet::new(magnet);
        if let Ok(m) = magnet {
            println!("{:#?}", m);

            let trackers: Vec<_> =
                m.tr.into_iter()
                    .map(|x| {
                        println!("{:#?}", urlencoding::decode(&x));
                        // http that works
                        // let tr = "p4p.arenabg.com:1337";

                        // explodie is the only UDP address
                        // that the announce works. why?
                        let tr = "explodie.org:6969";
                        tr
                        // let tr = urlencoding::decode(&x).unwrap();
                        // println!("raw {:#?}", tr);
                        // let tr = tr.replace("/announce", "");
                        // if tr.starts_with("udp://") {
                        //     let tr = tr.replace("udp://", "");
                        //     return tr;
                        // } else {
                        //     let tr = tr.replace("http://", "");
                        //     return tr;
                        // }
                    })
                    .collect();

            println!("trackers {:#?}", trackers);
            let client = Client::connect(trackers).unwrap();
            println!("client {:#?}", client);

            let mut hasher = Sha1::new();

            println!("hash_url is {:#?}", m.xt);

            let infohash = hex::decode(m.xt.clone().unwrap()).unwrap();

            println!("hash is {:#?}", infohash);

            hasher.update(infohash);

            let infohash: [u8; 20] = hasher.finalize().into();

            println!("infohash is {:#?}", infohash);

            client.announce_exchange(infohash).unwrap();
        }
    }
}
