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
    use urlencoding::decode;

    use super::*;
    use crate::tracker::client::Client;
    // http://bt1.archive.org:6969/announce?info_hash=%ac%c3%b2%e43%d7%c7GZ%bbYA%b5h%1c%b7%a1%ea%26%e2&peer_id=ABCDEFGHIJKLMNOPQRST&ip=80.11.255.166&port=6881&downloaded=0&left=970

    #[test]
    fn udp() {
        let magnet = "magnet:?xt=urn:btih:56BC861F42972DEA863AE853362A20E15C7BA07E&dn=Rust%20for%20Rustaceans%3A%20Idiomatic%20Programming&tr=udp%3A%2F%2Ftracker.coppersurfer.tk%3A6969%2Fannounce&tr=udp%3A%2F%2Ftracker.openbittorrent.com%3A6969%2Fannounce&tr=udp%3A%2F%2F9.rarbg.to%3A2710%2Fannounce&tr=udp%3A%2F%2F9.rarbg.me%3A2780%2Fannounce&tr=udp%3A%2F%2F9.rarbg.to%3A2730%2Fannounce&tr=udp%3A%2F%2Ftracker.opentrackr.org%3A1337&tr=http%3A%2F%2Fp4p.arenabg.com%3A1337%2Fannounce&tr=udp%3A%2F%2Ftracker.torrent.eu.org%3A451%2Fannounce&tr=udp%3A%2F%2Ftracker.tiny-vps.com%3A6969%2Fannounce&tr=udp%3A%2F%2Fopen.stealth.si%3A80%2Fannounce";
        let magnet = Magnet::new(magnet);
        if let Ok(m) = magnet {
            println!("{:#?}", m);
            // println!("");

            let trackers: Vec<_> =
                m.tr.into_iter()
                    .map(|x| {
                        // working with http but not udp
                        let tracker = decode("http%3A%2F%2Fp4p.arenabg.com%3A1337%2Fannounce")
                            .unwrap()
                            .to_string();
                        // let tracker = decode(x.as_str()).unwrap();
                        let tracker = tracker.replace("/announce", "");
                        let tracker = tracker[7..].to_string();
                        tracker
                    })
                    .collect();

            println!("trackers {:#?}", trackers);

            let client = Client::connect(trackers).unwrap();
            println!("client {:#?}", client);

            let mut buf = [0u8; 20];
            let infohash = hex::decode(m.xt.clone().unwrap()).unwrap();

            for i in 0..20 {
                buf[i] = infohash[i];
            }

            client.announce_exchange(buf).unwrap();
        }
    }
}
