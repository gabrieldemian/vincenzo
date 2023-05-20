use tokio::{
    select,
    sync::mpsc::{Receiver, Sender},
};

use crate::frontend::FrontendMessage;

#[derive(Debug)]
pub enum BackendMessage {
    Quit,
}

pub struct Backend {
    pub tx: Sender<BackendMessage>,
    pub rx: Receiver<BackendMessage>,
}

impl Backend {
    pub fn new(tx: Sender<BackendMessage>, rx: Receiver<BackendMessage>) -> Self {
        Self { tx, rx }
    }

    pub async fn daemon<'a>(&mut self, tx_app: Sender<FrontendMessage<'a>>) {
        loop {
            select! {
                e = self.rx.recv() => {
                    match e.unwrap() {
                        BackendMessage::Quit => return,
                    }
                }
            }
        }
    }
}
