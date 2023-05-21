use actix::prelude::*;
use frontend::Frontend;
use models::backend::Backend;

pub mod frontend;
pub mod models;
pub mod torrent_list;

fn main() -> Result<(), std::io::Error> {
    pretty_env_logger::init();
    let system = System::new();

    let fr_execution = async {
        Frontend::create(|ctx| {
            let bk_addr = Backend::new(ctx.address().recipient()).start();
            Frontend::new(bk_addr.recipient()).unwrap()
        });
    };

    // spawn OS thread
    let arbiter = Arbiter::new();
    arbiter.spawn(fr_execution);

    system.run()?;
    Ok(())
}
