use actix::prelude::*;

#[derive(Message)]
#[rtype(result = "()")]
pub enum BackendMessage {
    Quit,
}

pub struct Backend;

impl Actor for Backend {
    type Context = SyncContext<Self>;
}

impl Backend {
    pub fn new() -> Self {
        Self {}
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
        }
    }
}
