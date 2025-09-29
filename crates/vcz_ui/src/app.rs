use std::time::Duration;

use futures::{SinkExt, Stream, StreamExt};
use tokio::{
    net::TcpStream,
    select, spawn,
    sync::mpsc::{UnboundedReceiver, UnboundedSender},
    time::timeout,
};
use tokio_util::codec::Framed;
use tracing::debug;
use vcz_lib::{
    config::CONFIG,
    daemon_wire::{DaemonCodec, Message},
};

use crate::{
    action::Action,
    error::Error,
    pages::{Page, torrent_list::TorrentList},
    tui::Tui,
};

pub struct App {
    pub is_detached: bool,
    pub tx: UnboundedSender<Action>,
    should_quit: bool,
    page: TorrentList<'static>,
}

impl App {
    pub fn is_detched(mut self, v: bool) -> Self {
        self.is_detached = v;
        self
    }

    pub fn new(tx: UnboundedSender<Action>) -> Self {
        let page = TorrentList::new(tx.clone());

        App { should_quit: false, tx, page, is_detached: false }
    }

    pub async fn run(
        &mut self,
        mut rx: UnboundedReceiver<Action>,
    ) -> Result<(), Error> {
        let mut i = 0;

        let socket = loop {
            match timeout(
                Duration::from_millis(100),
                TcpStream::connect(CONFIG.daemon_addr),
            )
            .await
            {
                Ok(Ok(v)) => break v,
                _ => {
                    i += 1;
                    if i > 10 {
                        return Err(Error::DaemonNotRunning(
                            CONFIG.daemon_addr,
                        ));
                    }
                    tokio::time::sleep(Duration::from_millis(150)).await;
                }
            }
        };

        let mut tui = Tui::new()?;
        tui.run()?;

        let tx = self.tx.clone();

        // spawn event loop to listen to messages sent by the daemon
        let socket = Framed::new(socket, DaemonCodec);
        let (mut sink, stream) = socket.split();
        let _tx = self.tx.clone();

        let handle = spawn(async move {
            let _ = Self::listen_daemon(_tx, stream).await;
        });

        loop {
            let e = tui.next().await?;
            let a = self.page.handle_event(e);
            let _ = tx.send(a);

            while let Ok(action) = rx.try_recv() {
                match action {
                    Action::Render => {
                        let _ = tui.draw(|f| {
                            self.page.draw(f);
                        });
                    }
                    Action::Quit => {
                        if !self.is_detached {
                            let _ = sink.send(Message::Quit).await;
                        }
                        handle.abort();
                        tui.cancel();
                        self.should_quit = true;
                    }
                    Action::NewTorrent(magnet) => {
                        sink.send(Message::NewTorrent(magnet.clone())).await?;
                    }
                    Action::DeleteTorrent(info_hash) => {
                        sink.send(Message::DeleteTorrent(info_hash)).await?;
                    }
                    _ => self.page.handle_action(action),
                }
            }

            if self.should_quit {
                sink.send(Message::FrontendQuit).await?;
                break;
            }
        }

        Ok(())
    }

    /// Listen to the messages sent by the daemon via TCP,
    /// when we receive a message, we send it to ourselves
    /// via mpsc [`Action`]. For example, when we receive
    /// a TorrentState message from the daemon, we forward it to ourselves.
    pub async fn listen_daemon<
        T: Stream<Item = Result<Message, vcz_lib::error::Error>> + Unpin,
    >(
        tx: UnboundedSender<Action>,
        mut stream: T,
    ) {
        loop {
            select! {
                Some(Ok(msg)) = stream.next() => {
                    match msg {
                        Message::TorrentState(torrent_state) => {
                            let _ = tx.send(Action::TorrentState(torrent_state));
                        }
                        Message::TorrentStates(torrent_states) => {
                            let _ = tx.send(Action::TorrentStates(torrent_states));
                        }
                        Message::Quit => {
                            debug!("ui Quit");
                            let _ = tx.send(Action::Quit);
                            break;
                        }
                        Message::TogglePause(torrent) => {
                            let _ = tx.send(Action::TogglePause(torrent));
                        }
                        _ => {}
                    }
                }
                else => break
            }
        }
    }
}
