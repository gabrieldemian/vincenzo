pub mod torrent_list;
use clap::Parser;
use futures::{FutureExt, StreamExt};
use tracing::info;

use std::{
    io::{self, Stdout},
    sync::Arc,
    time::Duration,
};
use tokio::{
    select, spawn,
    sync::{mpsc, RwLock},
    time::interval,
};

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

use crate::{
    cli::Args,
    config::Config,
    disk::DiskMsg,
    torrent::{Torrent, TorrentCtx, TorrentMsg},
};

#[derive(Clone, Debug)]
pub struct AppStyle {
    pub base_style: Style,
    pub highlight_bg: Style,
    pub highlight_fg: Style,
    pub success: Style,
    pub error: Style,
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
        }
    }
}

#[derive(Debug, Clone)]
pub enum FrMsg {
    Quit,
    AddTorrent(Arc<TorrentCtx>),
    NewTorrent(String),
}

pub struct Frontend<'a> {
    pub style: AppStyle,
    pub ctx: Arc<FrontendCtx>,
    torrent_ctxs: RwLock<Vec<Arc<TorrentCtx>>>,
    disk_tx: mpsc::Sender<DiskMsg>,
    terminal: Terminal<CrosstermBackend<Stdout>>,
    torrent_list: TorrentList<'a>,
    config: Config,
}

pub struct FrontendCtx {
    pub fr_tx: mpsc::Sender<FrMsg>,
}

impl<'a> Frontend<'a> {
    pub fn new(fr_tx: mpsc::Sender<FrMsg>, disk_tx: mpsc::Sender<DiskMsg>, config: Config) -> Self {
        let stdout = io::stdout();
        let style = AppStyle::new();
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend).unwrap();

        let ctx = Arc::new(FrontendCtx { fr_tx });
        let torrent_list = TorrentList::new(ctx.clone());

        Frontend {
            config,
            terminal,
            torrent_list,
            ctx,
            disk_tx,
            style,
            torrent_ctxs: RwLock::new(Vec::new()),
        }
    }

    /// Run the UI event loop
    pub async fn run(&mut self, mut fr_rx: mpsc::Receiver<FrMsg>) -> Result<(), std::io::Error> {
        let mut reader = EventStream::new();

        // setup terminal
        let mut stdout = io::stdout();
        enable_raw_mode()?;
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;

        let mut tick_interval = interval(Duration::from_secs(1));

        let original_hook = std::panic::take_hook();

        std::panic::set_hook(Box::new(move |panic| {
            Self::reset_terminal();
            original_hook(panic);
        }));

        loop {
            let event = reader.next().fuse();

            select! {
                // Repaint UI every 1 second
                _ = tick_interval.tick() => {
                    self.torrent_list.draw(&mut self.terminal).await;
                }
                event = event => {
                    match event {
                        Some(Ok(event)) => {
                            if let crossterm::event::Event::Key(k) = event {
                                self.torrent_list.keybindings(k.code, &mut self.terminal).await;
                            }
                        }
                        _ => break
                    }
                }
                Some(msg) = fr_rx.recv() => {
                    match msg {
                        FrMsg::Quit => {
                            let _ = self.stop().await;
                            return Ok(());
                        },
                        FrMsg::AddTorrent(torrent_ctx) => {
                            self.add_torrent(torrent_ctx).await;
                            self.torrent_list.draw(&mut self.terminal).await;
                        },
                        FrMsg::NewTorrent(magnet) => {
                            self.new_torrent(&magnet).await;
                        }
                    }
                }
            }
        }

        Ok(())
    }

    // Add a torrent that is already initialized, this is called when the user
    // uses the magnet flag on the CLI, the Torrent is created on main.rs.
    async fn add_torrent(&mut self, torrent_ctx: Arc<TorrentCtx>) {
        let mut v = self.torrent_ctxs.write().await;
        v.push(torrent_ctx.clone());
        self.torrent_list.update_ctx(torrent_ctx).await;
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

    async fn stop(&self) {
        Self::reset_terminal();

        let torrent_ctxs = self.torrent_ctxs.read().await;

        // tell all torrents that we are gracefully shutting down,
        // each torrent will kill their peers tasks, and their tracker task
        for torrent_ctx in torrent_ctxs.iter() {
            let torrent_tx = torrent_ctx.tx.clone();

            spawn(async move {
                let _ = torrent_tx.send(TorrentMsg::Quit).await;
            });
        }

        let torrent_ctxs = self.torrent_ctxs.read().await;

        if torrent_ctxs.is_empty() {
            self.disk_tx.send(DiskMsg::Quit).await.unwrap();
        }
    }

    // Create a Torrent, and then Add it. This will be called when the user
    // adds a torrent using the UI.
    async fn new_torrent(&mut self, magnet: &str) {
        info!("download_dir {}", self.config.download_dir);

        let mut torrent = Torrent::new(self.disk_tx.clone(), magnet, &self.config.download_dir);
        let _ = self.add_torrent(torrent.ctx.clone()).await;

        let args = Args::parse();
        let mut listen = self.config.listen;

        if args.listen.is_some() {
            listen = args.listen;
        }

        spawn(async move {
            torrent.start_and_run(listen).await.unwrap();
            torrent.disk_tx.send(DiskMsg::Quit).await.unwrap();
        });
    }
}
