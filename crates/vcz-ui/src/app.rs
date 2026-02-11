use crate::{
    action::{self, Action},
    components::footer,
    error::Error,
    pages::{Empty, Page, TorrentList},
    tui::Tui,
};
use futures::{SinkExt, Stream, StreamExt};
use ratatui::{
    Terminal,
    layout::{Constraint, Layout},
    prelude::CrosstermBackend,
};
use std::{sync::Arc, time::Duration};
use tokio::{
    net::TcpStream,
    select, spawn,
    sync::mpsc::{UnboundedReceiver, UnboundedSender},
    time::timeout,
};
use tokio_util::codec::Framed;
use tracing::debug;
use vcz_lib::{
    config::ResolvedConfig,
    daemon_wire::{DaemonCodec, Message},
};

pub struct App {
    pub is_detached: bool,
    pub tx: UnboundedSender<Action>,
    pub config: Arc<ResolvedConfig>,
    should_quit: bool,
    page: Box<dyn Page>,
}

impl App {
    pub fn is_detched(mut self, v: bool) -> Self {
        self.is_detached = v;
        self
    }

    pub fn new(
        config: Arc<ResolvedConfig>,
        tx: UnboundedSender<Action>,
    ) -> Self {
        let page = Box::new(Empty::new(tx.clone()));
        App { config, should_quit: false, tx, page, is_detached: false }
    }

    pub async fn run(
        &mut self,
        mut rx: UnboundedReceiver<Action>,
    ) -> Result<(), Error> {
        let mut i = 0;

        let socket = loop {
            match timeout(
                Duration::from_millis(100),
                TcpStream::connect(self.config.daemon_addr),
            )
            .await
            {
                Ok(Ok(v)) => break v,
                _ => {
                    i += 1;
                    if i > 10 {
                        return Err(Error::DaemonNotRunning(
                            self.config.daemon_addr,
                        ));
                    }
                    tokio::time::sleep(Duration::from_millis(150)).await;
                }
            }
        };

        let backend = CrosstermBackend::new(std::io::stdout());
        let terminal = Terminal::new(backend)?;
        let mut tui = Tui::new(terminal)?;
        let tx = self.tx.clone();
        tui.init()?;

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
                    Action::ChangePage(page) => match page {
                        action::Page::Empty => {
                            let page = Empty::new(tx.clone());
                            self.page = Box::new(page);
                        }
                        action::Page::TorrentList => {
                            let page = TorrentList::new(tx.clone());
                            self.page = Box::new(page);
                        }
                    },
                    Action::Render => {
                        // there is a layout that is persisted between
                        // all pages, the footer with a text of the
                        // keybindings. Split the layout in 2 and only
                        // send the necessary part to the page.
                        let _ = tui.terminal.draw(|f| {
                            let chunks = Layout::vertical([
                                Constraint::Percentage(100),
                                Constraint::Min(1),
                            ])
                            .split(f.area());

                            f.render_widget(footer(), chunks[1]);
                            self.page.draw(f, chunks[0]);
                        });
                    }
                    Action::Quit => {
                        if !self.is_detached {
                            let _ = sink.send(Message::Quit).await;
                        }
                        handle.abort();
                        tui.cancel()?;
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

        tui.exit()?;
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
                        Message::Init(has_torrents) => {
                            let _ = tx.send(
                                Action::ChangePage(
                                    if has_torrents {
                                        action::Page::TorrentList
                                    } else {
                                        action::Page::Empty
                                    }
                                )
                            );
                        }
                        Message::TorrentState(v) => {
                            let _ = tx.send(Action::TorrentState(v));
                        }
                        Message::TorrentStates(v) => {
                            let _ = tx.send(Action::TorrentStates(v));
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
