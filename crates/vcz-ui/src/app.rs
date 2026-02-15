use crate::{
    Input, Key,
    action::{self, Action},
    components::footer,
    error::Error,
    pages::{Empty, Info, Page, TorrentList},
    tui::Tui,
    widgets::Menu,
};
use futures::{SinkExt, Stream, StreamExt};
use ratatui::{
    Terminal,
    layout::{Constraint, Layout},
    prelude::CrosstermBackend,
    style::Stylize,
};
use std::{sync::Arc, time::Duration};
use tokio::{
    net::TcpStream,
    select, spawn,
    sync::mpsc::{UnboundedReceiver, UnboundedSender},
    time::timeout,
};
use tokio_util::codec::Framed;
use vcz_lib::{
    config::ResolvedConfig,
    daemon_wire::{DaemonCodec, Message},
};

/// Global state shared between all pages.
#[derive(Clone, Debug)]
pub struct State {
    /// If components should be dimmed.
    pub should_dim: bool,
    /// Show network widget on page torrent list.
    pub show_network: bool,
    pub config: Arc<ResolvedConfig>,
}

impl State {
    pub fn new(config: Arc<ResolvedConfig>) -> Self {
        Self { config, should_dim: false, show_network: false }
    }
}

pub struct App {
    pub is_detached: bool,
    tx: UnboundedSender<Action>,
    should_quit: bool,
    state: State,
    page: Box<dyn Page>,
    menu: Menu,
}

impl App {
    pub fn new(
        config: Arc<ResolvedConfig>,
        tx: UnboundedSender<Action>,
    ) -> Self {
        let page = Box::new(Empty::new(tx.clone()));
        let menu = Menu::new(tx.clone());
        App {
            state: State::new(config),
            should_quit: false,
            menu,
            tx,
            page,
            is_detached: false,
        }
    }

    pub async fn run(
        &mut self,
        mut rx: UnboundedReceiver<Action>,
    ) -> Result<(), Error> {
        let mut i = 0;

        let socket = loop {
            match timeout(
                Duration::from_millis(100),
                TcpStream::connect(self.state.config.daemon_addr),
            )
            .await
            {
                Ok(Ok(v)) => break v,
                _ => {
                    i += 1;
                    if i > 10 {
                        return Err(Error::DaemonNotRunning(
                            self.state.config.daemon_addr,
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
            let a = tui.next().await?;
            self.handle_action(&a);
            let _ = tx.send(a);
            let is_empty = self.page.id() == crate::action::Page::Empty;

            while let Ok(action) = rx.try_recv() {
                match action {
                    Action::ChangePage(page) => match page {
                        action::Page::Empty => {
                            let page = Empty::new(tx.clone());
                            self.page = Box::new(page);
                        }
                        action::Page::Info => {
                            let page =
                                Info::new(tx.clone(), &self.state.config);
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
                            if is_empty {
                                self.page.draw(f, f.area(), &mut self.state);
                            } else {
                                let chunks = Layout::vertical([
                                    Constraint::Min(2),
                                    Constraint::Percentage(100),
                                    Constraint::Min(1),
                                ])
                                .split(f.area());
                                self.menu.draw(f, chunks[0], &mut self.state);
                                self.page.draw(f, chunks[1], &mut self.state);
                                let mut footer = footer();
                                if self.state.should_dim {
                                    footer = footer.dim();
                                }
                                f.render_widget(footer, chunks[2]);
                            }
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
                    _ => self.page.handle_action(action, &mut self.state),
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

    fn handle_action(&mut self, action: &Action) {
        let Action::Input(input) = action else { return };
        let is_empty = self.page.id() == crate::action::Page::Empty;

        match input {
            Input { key: Key::Tab, shift: false, .. } if !is_empty => {
                self.menu.next();
            }
            Input { key: Key::Tab, shift: true, .. } if !is_empty => {
                self.menu.previous();
            }
            _ => {}
        }
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
                            if !has_torrents {
                                let _ = tx.send(
                                    Action::ChangePage(
                                        action::Page::Info
                                    )
                                );
                            }
                        }
                        Message::TorrentState(v) => {
                            let _ = tx.send(Action::TorrentState(v));
                        }
                        Message::TorrentStates(v) => {
                            let _ = tx.send(Action::TorrentStates(v));
                        }
                        Message::Quit => {
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
