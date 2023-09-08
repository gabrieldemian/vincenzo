use tracing::debug;
pub mod error;
pub mod torrent_list;
use futures::{stream::SplitStream, FutureExt, SinkExt, StreamExt};
use hashbrown::HashMap;
use tokio_util::codec::Framed;

use std::{
    io::{self, Stdout},
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

use vcz_lib::{
    daemon_wire::{DaemonCodec, Message},
    error::Error,
    torrent::TorrentMsg,
    FrMsg, TorrentState,
};

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

pub struct Frontend<'a> {
    pub style: AppStyle,
    pub ctx: Arc<FrontendCtx>,
    pub torrent_list: TorrentList<'a>,
    torrent_txs: HashMap<[u8; 20], mpsc::Sender<TorrentMsg>>,
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

pub struct FrontendCtx {
    pub fr_tx: mpsc::Sender<FrMsg>,
}

impl<'a> Frontend<'a> {
    pub fn new(fr_tx: mpsc::Sender<FrMsg>) -> Self {
        let stdout = io::stdout();
        let style = AppStyle::new();
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend).unwrap();

        let ctx = Arc::new(FrontendCtx { fr_tx });
        let torrent_list = TorrentList::new(ctx.clone());

        Frontend {
            terminal,
            torrent_list,
            torrent_txs: HashMap::new(),
            ctx,
            style,
        }
    }

    /// Listen to the messages sent by the daemon on a TCP socket,
    /// when we receive a message, we send a message to ourselves
    /// that corresponds to the `FrMsg`. For example, when we receive
    /// a Draw message from the daemon, we send a Draw message to `run`
    pub async fn listen_daemon(
        fr_tx: mpsc::Sender<FrMsg>,
        mut sink: SplitStream<Framed<TcpStream, DaemonCodec>>,
    ) -> Result<(), Error> {
        loop {
            select! {
                Some(Ok(msg)) = sink.next() => {
                    match msg {
                        Message::TorrentState(torrent_info) => {
                            let _ = fr_tx.send(FrMsg::Draw(torrent_info)).await;
                        }
                        Message::Quit => {
                            let _ = fr_tx.send(FrMsg::Quit).await;
                            return Ok(())
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    /// Run the UI event loop
    pub async fn run(&mut self, mut fr_rx: mpsc::Receiver<FrMsg>) -> Result<(), Error> {
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
        let socket = TcpStream::connect("127.0.0.1:3030").await.unwrap();

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
                        FrMsg::Quit => {
                            self.stop(&mut sink).await?;
                            break 'outer;
                        },
                        FrMsg::Draw(torrent_info) => {
                            self.torrent_list
                                .torrent_infos
                                .insert(torrent_info.info_hash, torrent_info);

                            self.torrent_list.draw(&mut self.terminal).await;
                        },
                        FrMsg::NewTorrent(magnet) => {
                            self.new_torrent(&magnet, &mut sink).await?;
                        }
                        FrMsg::TogglePause(id) => {
                            let tx = self.torrent_txs.get(&id).ok_or(Error::TorrentDoesNotExist)?;
                            tx.send(TorrentMsg::TogglePause).await?;
                        }
                    }
                }
            }
        }
        debug!("ui quitted loop");

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
        debug!("ui on stop fn");
        sink.send(Message::Quit)
            .await
            .map_err(|_| Error::SendErrorTcp)?;

        debug!("ui sent quit");

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
