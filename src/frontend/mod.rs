pub mod torrent_list;
use futures::{FutureExt, StreamExt};

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
    prelude::Backend,
    style::{Color, Style},
    Terminal,
};

use torrent_list::TorrentList;

use crate::{
    cli::Args,
    disk::DiskMsg,
    torrent::{TorrentCtx, TorrentMsg},
};

#[derive(Clone, Debug)]
pub struct AppStyle {
    pub base_style: Style,
    pub selected_style: Style,
    pub normal_style: Style,
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
            selected_style: Style::default().bg(Color::LightBlue).fg(Color::DarkGray),
            normal_style: Style::default().fg(Color::LightBlue),
        }
    }
}

#[derive(Debug, Clone)]
pub enum FrMsg {
    Quit,
    AddTorrent(Arc<TorrentCtx>),
}

pub struct Frontend<'a> {
    pub style: AppStyle,
    pub ctx: Arc<FrontendCtx>,
    torrent_ctxs: RwLock<Vec<Arc<TorrentCtx>>>,
    disk_tx: mpsc::Sender<DiskMsg>,
    terminal: Terminal<CrosstermBackend<Stdout>>,
    torrent_list: TorrentList<'a>,
}

pub struct FrontendCtx {
    pub fr_tx: mpsc::Sender<FrMsg>,
}

impl<'a> Frontend<'a> {
    pub fn new(fr_tx: mpsc::Sender<FrMsg>, disk_tx: mpsc::Sender<DiskMsg>) -> Self {
        let stdout = io::stdout();
        let style = AppStyle::new();
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend).unwrap();

        let ctx = Arc::new(FrontendCtx { fr_tx });
        let torrent_list = TorrentList::new(ctx.clone());

        Frontend {
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

        loop {
            let event = reader.next().fuse();

            select! {
                // Update UI every 1 second
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
                        }
                        FrMsg::AddTorrent(torrent_ctx) => {
                            self.add_torrent(torrent_ctx).await;
                            self.torrent_list.draw(&mut self.terminal).await;
                        },
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

    async fn stop(&self) {
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
    // async fn _new_torrent(&mut self, magnet: &str) {
    //     let args = Args::parse();
    //     let mut torrent = Torrent::new(self.disk_tx.clone(), magnet, &args.download_dir);
    //     let _ = self.add_torrent(torrent.ctx.clone()).await;
    //
    //     spawn(async move {
    //         torrent.start_and_run(args.listen).await.unwrap();
    //     });
    // }
}
