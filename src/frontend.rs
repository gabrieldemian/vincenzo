use std::{
    io::{self, Stdout},
    marker::PhantomData,
    time::{Duration, Instant},
};

use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use tokio::sync::mpsc::{Receiver, Sender};
use tui::{
    backend::CrosstermBackend,
    style::{Color, Style},
    Terminal,
};

use crate::{models::backend::BackendMessage, torrent_list::TorrentList};

pub struct AppStyle {
    pub base_style: Style,
    pub selected_style: Style,
    pub normal_style: Style,
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
pub enum FrontendMessage<'a> {
    Quit,
    P(PhantomData<&'a &'a str>),
}

// actor
pub struct Frontend<'a> {
    pub style: AppStyle,
    pub page: TorrentList<'a>,
    pub should_close: bool,
    pub tick_rate: Duration,
    pub timeout: Duration,
    pub terminal: Terminal<CrosstermBackend<Stdout>>,
    pub rx: Receiver<FrontendMessage<'a>>,
    pub tx: Sender<FrontendMessage<'a>>,
    pub tx_backend: Sender<BackendMessage>,
}

// handle
pub struct FrontendHandle<'a> {
    pub tx: Sender<FrontendMessage<'a>>,
}

impl<'a> Frontend<'a> {
    pub fn new(
        rx: Receiver<FrontendMessage<'a>>,
        tx: Sender<FrontendMessage<'a>>,
        tx_backend: Sender<BackendMessage>,
    ) -> Result<Frontend<'a>, std::io::Error> {
        let style = AppStyle::new();
        let page = TorrentList::new();

        // setup terminal
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;

        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend)?;

        Ok(Frontend {
            style,
            page,
            should_close: false,
            terminal,
            tick_rate: Duration::from_millis(250),
            timeout: Duration::from_millis(0),
            rx,
            tx,
            tx_backend,
        })
    }

    pub async fn run(
        &mut self,
        // tx: Sender<AppMessage<'a>>,
        // tx_network: Sender<GlobalEvent>,
    ) -> Result<(), std::io::Error> {
        let mut last_tick = Instant::now();

        loop {
            self.terminal.draw(|f| self.page.draw(f)).unwrap();

            self.timeout = self
                .tick_rate
                .checked_sub(last_tick.elapsed())
                .unwrap_or_else(|| Duration::from_secs(0));

            if event::poll(self.timeout).unwrap() {
                if let Event::Key(k) = event::read().unwrap() {
                    self.page
                        .keybindings(k.code, &self.tx, &self.tx_backend)
                        .await;
                }
            }

            // try_recv is non-blocking
            if let Ok(msg) = self.rx.try_recv() {
                self.handle_message(msg).await;
            }

            if last_tick.elapsed() >= self.tick_rate {
                last_tick = Instant::now();
            }

            if self.should_close {
                return Ok(());
            }
        }
    }

    async fn handle_message(&mut self, msg: FrontendMessage<'a>) {
        match msg {
            FrontendMessage::Quit => {
                disable_raw_mode().unwrap();
                execute!(
                    self.terminal.backend_mut(),
                    LeaveAlternateScreen,
                    DisableMouseCapture
                )
                .unwrap();
                self.terminal.show_cursor().unwrap();
                self.should_close = true;
                // send message to `Backend`
                let _ = self.tx_backend.send(BackendMessage::Quit).await;
            }
            _ => {}
        }
    }
}

impl<'a> FrontendHandle<'a>
where
    'a: 'static,
{
    pub fn new(
        tx: Sender<FrontendMessage<'a>>,
        rx: Receiver<FrontendMessage<'a>>,
        tx_backend: Sender<BackendMessage>,
    ) -> Self {
        let actor = Frontend::new(rx, tx.clone(), tx_backend);

        tokio::spawn(async move { actor.unwrap().run().await });

        Self { tx }
    }
}
