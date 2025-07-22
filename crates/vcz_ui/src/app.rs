use futures::{SinkExt, Stream, StreamExt};
use tokio::{
    net::TcpStream,
    select, spawn,
    sync::mpsc::{self, unbounded_channel, UnboundedReceiver, UnboundedSender},
};
use tokio_util::codec::Framed;
use tracing::debug;
use vincenzo::{
    config::CONFIG,
    daemon_wire::{DaemonCodec, Message},
};

use crate::{
    action::{self, Action},
    error::Error,
    pages::{torrent_list::TorrentList, Page},
    tui::Tui,
};

pub struct App {
    pub is_detached: bool,
    pub tx: UnboundedSender<Action>,
    should_quit: bool,
    rx: Option<UnboundedReceiver<Action>>,
    page: Box<dyn Page>,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    pub fn is_detched(mut self, v: bool) -> Self {
        self.is_detached = v;
        self
    }

    pub fn new() -> Self {
        let (tx, rx) = unbounded_channel();

        let page = Box::new(TorrentList::new(tx.clone()));

        App { should_quit: false, tx, rx: Some(rx), page, is_detached: false }
    }

    pub async fn run(&mut self) -> Result<(), Error> {
        let mut tui = Tui::new()?;
        tui.run()?;

        let tx = self.tx.clone();
        let mut rx = std::mem::take(&mut self.rx).unwrap();

        let daemon_addr = CONFIG.daemon_addr;
        let socket = TcpStream::connect(daemon_addr).await.unwrap();

        // spawn event loop to listen to messages sent by the daemon
        let socket = Framed::new(socket, DaemonCodec);
        let (mut sink, stream) = socket.split();
        let _tx = self.tx.clone();

        let handle = spawn(async move {
            let _ = Self::listen_daemon(_tx, stream).await;
        });

        loop {
            // block until the next event
            let e = tui.next().await?;
            let a = self.page.get_action(e);
            let _ = tx.send(a);

            while let Ok(action) = rx.try_recv() {
                self.page.handle_action(&action);

                if let Action::Render = action {
                    let _ = tui.draw(|f| {
                        self.page.draw(f);
                    });
                }

                if let Action::Quit = action {
                    if !self.is_detached {
                        let _ = sink.send(Message::Quit).await;
                    }
                    handle.abort();
                    tui.cancel();
                    self.should_quit = true;
                }

                if let Action::ChangePage(component) = action {
                    self.handle_change_component(component)?
                }

                if let Action::NewTorrent(magnet) = action {
                    sink.send(Message::NewTorrent(magnet.to_owned())).await?;
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
        T: Stream<Item = Result<Message, std::io::Error>> + Unpin,
    >(
        tx: mpsc::UnboundedSender<Action>,
        mut stream: T,
    ) {
        debug!("ui listen_daemon");

        loop {
            select! {
                Some(Ok(msg)) = stream.next() => {
                    match msg {
                        Message::TorrentState(torrent_state) => {
                            let _ = tx.send(Action::TorrentState(torrent_state));
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

    /// Handle the logic to render another component on the screen, after
    /// receiving an [`Action::ChangePage`]
    fn handle_change_component(
        &mut self,
        page: action::Page,
    ) -> Result<(), Error> {
        self.page = match page {
            action::Page::TorrentList => {
                Box::new(TorrentList::new(self.tx.clone()))
            }
        };
        Ok(())
    }
}
