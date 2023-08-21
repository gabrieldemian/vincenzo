pub mod torrent_list;
use clap::Parser;
use futures::{FutureExt, StreamExt};
use hashbrown::HashMap;
use tracing::info;

use std::{
    io::{self, Stdout},
    sync::Arc,
};
use tokio::{select, spawn, sync::mpsc};

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
    torrent::{Stats, Torrent, TorrentMsg, TorrentStatus},
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
    Draw([u8; 20], TorrentInfo),
    NewTorrent(String),
}

#[derive(Debug, Clone, Default)]
pub struct TorrentInfo {
    pub name: String,
    pub stats: Stats,
    pub status: TorrentStatus,
    pub downloaded: u64,
    pub download_rate: u64,
    pub uploaded: u64,
    pub size: u64,
}

pub struct Frontend<'a> {
    pub style: AppStyle,
    pub ctx: Arc<FrontendCtx>,
    torrent_txs: HashMap<[u8; 20], mpsc::Sender<TorrentMsg>>,
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
            torrent_txs: HashMap::new(),
            ctx,
            disk_tx,
            style,
        }
    }

    /// Run the UI event loop
    pub async fn run(&mut self, mut fr_rx: mpsc::Receiver<FrMsg>) -> Result<(), std::io::Error> {
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

        loop {
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
                            let _ = self.stop().await;
                            return Ok(());
                        },
                        FrMsg::Draw(info_hash, torrent_info) => {
                            info!("draw {torrent_info:#?}");

                            self.torrent_list
                                .torrent_infos
                                .insert(info_hash, torrent_info);

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

    // Create a Torrent, and then Add it. This will be called when the user
    // adds a torrent using the UI.
    async fn new_torrent(&mut self, magnet: &str) {
        let mut torrent = Torrent::new(self.disk_tx.clone(), self.ctx.fr_tx.clone(), magnet);
        let info_hash = torrent.ctx.info_hash.clone();

        self.torrent_txs.insert(info_hash, torrent.ctx.tx.clone());

        let torrent_info_l = TorrentInfo {
            name: torrent.ctx.info.read().await.name.clone(),
            ..Default::default()
        };

        self.torrent_list
            .torrent_infos
            .insert(info_hash, torrent_info_l);

        let args = Args::parse();
        let mut listen = self.config.listen;

        if args.listen.is_some() {
            listen = args.listen;
        }

        spawn(async move {
            torrent.start_and_run(listen).await.unwrap();
            torrent.disk_tx.send(DiskMsg::Quit).await.unwrap();
        });

        self.torrent_list.draw(&mut self.terminal).await;
    }

    async fn stop(&mut self) {
        Self::reset_terminal();

        // tell all torrents that we are gracefully shutting down,
        // each torrent will kill their peers tasks, and their tracker task
        for (_, tx) in std::mem::take(&mut self.torrent_txs) {
            spawn(async move {
                let _ = tx.send(TorrentMsg::Quit).await;
            });
        }
    }
}
