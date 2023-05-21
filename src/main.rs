use actix::prelude::*;
use frontend::Frontend;
use models::backend::Backend;

pub mod frontend;
pub mod models;
pub mod torrent_list;

fn main() -> Result<(), String> {
    let system = System::new();
    pretty_env_logger::init();

    let fr_execution = async {
        let bk_addr = SyncArbiter::start(2, || Backend);
        let _fr_addr = Frontend::new(bk_addr.recipient())
            .expect("to call frontend::new")
            .start();
    };

    // spawn OS thread
    let arbiter = Arbiter::new();
    arbiter.spawn(fr_execution);

    // run event loops
    system.run().unwrap();
    Ok(())
}
