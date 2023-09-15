use tracing::debug;
pub mod error;
pub mod torrent_list;
use futures::{stream::SplitStream, FutureExt, SinkExt, StreamExt};
use hashbrown::HashMap;
use tokio_util::codec::Framed;

use std::{
    io::{self, Stdout},
    net::SocketAddr,
    sync::Arc,
};
use tokio::{net::TcpStream, select, spawn, sync::mpsc};

use crossterm::{
    self,
    event::{DisableMouseCapture, EnableMouseCapture, EventStream},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    style::{Color, Style},
    Terminal,
};

use torrent_list::TorrentList;

use vincenzo::{
    daemon_wire::{DaemonCodec, Message},
    error::Error,
    torrent::{TorrentMsg, TorrentState},
};

/// Messages used by [`UI`] for internal communication.
#[derive(Debug, Clone)]
pub enum UIMsg {
    NewTorrent(String),
    Draw(TorrentState),
    TogglePause([u8; 20]),
    Quit,
}

#[derive(Clone, Debug)]
pub struct AppStyle {
    pub base_style: Style,
    pub highlight_bg: Style,
    pub highlight_fg: Style,
    pub success: Style,
    pub error: Style,
    pub warning: Style,
}

impl Default for AppStyle {
    fn default() -> Self {
        Self::new()
    }
}

impl AppStyle {
    pub fn new() -> Self {
        AppStyle {
            base_style: Style::default().fg(Color::Gray),
            highlight_bg: Style::default().bg(Color::LightBlue).fg(Color::DarkGray),
            highlight_fg: Style::default().fg(Color::LightBlue),
            success: Style::default().fg(Color::LightGreen),
            error: Style::default().fg(Color::Red),
            warning: Style::default().fg(Color::Yellow),
        }
    }
}

/// The UI runs entirely on the terminal.
/// It will communicate with the [`Daemon`] occasionaly,
/// via TCP messages documented at [`DaemonCodec`].
pub struct UI<'a> {
    pub style: AppStyle,
    pub ctx: Arc<UICtx>,
    pub torrent_list: TorrentList<'a>,
    torrent_txs: HashMap<[u8; 20], mpsc::Sender<TorrentMsg>>,
    terminal: Terminal<CrosstermBackend<Stdout>>,
    /// If this UI process is running detached from the Daemon,
    /// in it's own process.
    /// If this is the case, we don't want to send a Quit message to
    /// the Daemon when we close the UI.
    pub is_detached: bool,
}

/// Context that is shared between all pages,
/// at the moment, there is only one page [`TorrentList`].
pub struct UICtx {
    pub fr_tx: mpsc::Sender<UIMsg>,
}

impl<'a> UI<'a> {
    pub fn new(fr_tx: mpsc::Sender<UIMsg>) -> Self {
        let stdout = io::stdout();
        let style = AppStyle::new();
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend).unwrap();

        let ctx = Arc::new(UICtx { fr_tx });
        let torrent_list = TorrentList::new(ctx.clone());

        UI {
            terminal,
            torrent_list,
            torrent_txs: HashMap::new(),
            ctx,
            style,
            is_detached: false,
        }
    }

    /// Listen to the messages sent by the daemon via TCP,
    /// when we receive a message, we send it to ourselves
    /// via mpsc [`UIMsg`]. For example, when we receive
    /// a Draw message from the daemon, we send a Draw message to `run`
    pub async fn listen_daemon(
        fr_tx: mpsc::Sender<UIMsg>,
        mut sink: SplitStream<Framed<TcpStream, DaemonCodec>>,
    ) -> Result<(), Error> {
        loop {
            select! {
                Some(Ok(msg)) = sink.next() => {
                    match msg {
                        Message::TorrentState(Some(torrent_info)) => {
                            let _ = fr_tx.send(UIMsg::Draw(torrent_info)).await;
                        }
                        Message::Quit => {
                            let _ = fr_tx.send(UIMsg::Quit).await;
                            break;
                        }
                        _ => {}
                    }
                }
                else => break,
            }
        }
        Ok(())
    }

    /// Run the UI event loop and connect with the Daemon
    pub async fn run(
        &mut self,
        mut fr_rx: mpsc::Receiver<UIMsg>,
        daemon_addr: SocketAddr,
    ) -> Result<(), Error> {
        let mut reader = EventStream::new();

        // setup terminal
        let mut stdout = io::stdout();
        enable_raw_mode()?;
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;

        let original_hook = std::panic::take_hook();

        std::panic::set_hook(Box::new(move |panic| {
            Self::reset_terminal();
            original_hook(panic);
        }));

        self.torrent_list.draw(&mut self.terminal).await;

        let fr_tx = self.ctx.fr_tx.clone();

        let socket = TcpStream::connect(daemon_addr).await.unwrap();

        debug!("ui connected to daemon on {:?}", socket.local_addr());

        let socket = Framed::new(socket, DaemonCodec);
        let (mut sink, stream) = socket.split();

        spawn(async move {
            Self::listen_daemon(fr_tx, stream).await.unwrap();
        });

        'outer: loop {
            let event = reader.next().fuse();

            select! {
                event = event => {
                    match event {
                        Some(Ok(event)) => {
                            if let crossterm::event::Event::Key(k) = event {
                                self.torrent_list.keybindings(k, &mut self.terminal).await;
                            }
                        }
                        _ => break
                    }
                }
                Some(msg) = fr_rx.recv() => {
                    match msg {
                        UIMsg::Quit => {
                            self.stop(&mut sink).await?;
                            break 'outer;
                        },
                        UIMsg::Draw(torrent_info) => {
                            self.torrent_list
                                .torrent_infos
                                .insert(torrent_info.info_hash, torrent_info);

                            self.torrent_list.draw(&mut self.terminal).await;
                        },
                        UIMsg::NewTorrent(magnet) => {
                            self.new_torrent(&magnet, &mut sink).await?;
                        }
                        UIMsg::TogglePause(id) => {
                            let tx = self.torrent_txs.get(&id).ok_or(Error::TorrentDoesNotExist)?;
                            tx.send(TorrentMsg::TogglePause).await?;
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Send a NewTorrent message to Daemon, it will answer with a Draw request
    /// with the newly added torrent state.
    async fn new_torrent<T>(&mut self, magnet: &str, sink: &mut T) -> Result<(), Error>
    where
        T: SinkExt<Message> + Sized + std::marker::Unpin,
    {
        sink.send(Message::NewTorrent(magnet.to_owned()))
            .await
            .map_err(|_| Error::SendErrorTcp)?;
        Ok(())
    }

    async fn stop<T>(&mut self, sink: &mut T) -> Result<(), Error>
    where
        T: SinkExt<Message> + Sized + std::marker::Unpin,
    {
        if !self.is_detached {
            sink.send(Message::Quit)
                .await
                .map_err(|_| Error::SendErrorTcp)?;
        }

        Self::reset_terminal();

        Ok(())
    }

    fn reset_terminal() {
        let stdout = io::stdout();
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend).unwrap();

        disable_raw_mode().unwrap();
        terminal.show_cursor().unwrap();
        execute!(
            terminal.backend_mut(),
            LeaveAlternateScreen,
            DisableMouseCapture
        )
        .unwrap();
    }
}
