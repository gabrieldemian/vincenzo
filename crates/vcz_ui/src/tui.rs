use crate::error::Error;
use std::{
    ops::{Deref, DerefMut},
    time::Duration,
};

use crossterm::event::EventStream;
use futures::{FutureExt, StreamExt};
use ratatui::{
    backend::CrosstermBackend as Backend,
    crossterm::{
        self, cursor,
        event::{EnableBracketedPaste, EnableMouseCapture},
        terminal::EnterAlternateScreen,
    },
};
use tokio::{
    sync::mpsc::{self, Receiver, Sender},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug)]
pub enum Event {
    TerminalEvent(ratatui::crossterm::event::Event),
    Tick,
    Render,
    Quit,
    Error,
}

pub struct Tui {
    pub terminal: ratatui::Terminal<Backend<std::io::Stdout>>,
    pub task: JoinHandle<()>,
    pub cancellation_token: CancellationToken,
    pub event_rx: Receiver<Event>,
    pub event_tx: Sender<Event>,
    pub mouse: bool,
    pub paste: bool,
}

impl Tui {
    const FRAME_RATE: f64 = 60.0;
    const TICK_RATE: f64 = 4.0;

    pub fn new() -> Result<Self, Error> {
        let terminal = ratatui::init();
        let (event_tx, event_rx) = mpsc::channel(50);
        let cancellation_token = CancellationToken::new();
        let task = tokio::spawn(async {});
        let mouse = false;
        let paste = false;

        Ok(Self {
            terminal,
            task,
            cancellation_token,
            event_rx,
            event_tx,
            mouse,
            paste,
        })
    }

    pub fn mouse(mut self, mouse: bool) -> Self {
        self.mouse = mouse;
        self
    }

    pub fn paste(mut self, paste: bool) -> Self {
        self.paste = paste;
        self
    }

    fn start(&mut self) {
        let tick_delay = Duration::from_secs_f64(1.0 / Self::TICK_RATE);
        let render_delay = Duration::from_secs_f64(1.0 / Self::FRAME_RATE);

        self.cancellation_token = CancellationToken::new();

        let cancellation_token = self.cancellation_token.clone();
        let event_tx = self.event_tx.clone();

        tokio::spawn(async move {
            let mut tick_interval = tokio::time::interval(tick_delay);
            let mut render_interval = tokio::time::interval(render_delay);

            loop {
                let tick_delay = tick_interval.tick();
                let render_delay = render_interval.tick();
                let mut reader = EventStream::default();

                tokio::select! {
                    event = reader.next().fuse() => {
                        if let Some(Ok(event)) = event {
                            event_tx.send(Event::TerminalEvent(event)).await?;
                        }
                    },
                    _ = tick_delay => {
                        event_tx.send(Event::Tick).await?;
                    },
                    _ = render_delay => {
                        event_tx.send(Event::Render).await?;
                    },
                    _ = cancellation_token.cancelled() => {
                        break;
                    }
                }
            }
            Ok::<(), Error>(())
        });
    }

    pub fn run(&mut self) -> Result<(), Error> {
        crossterm::terminal::enable_raw_mode().unwrap();
        crossterm::execute!(
            std::io::stderr(),
            EnterAlternateScreen,
            cursor::Hide
        )
        .unwrap();
        if self.mouse {
            crossterm::execute!(std::io::stderr(), EnableMouseCapture).unwrap();
        }
        if self.paste {
            crossterm::execute!(std::io::stderr(), EnableBracketedPaste)
                .unwrap();
        }
        self.start();
        Ok(())
    }

    pub fn cancel(&self) {
        self.cancellation_token.cancel();
        ratatui::restore();
    }

    pub async fn next(&mut self) -> Result<Event, Error> {
        self.event_rx.recv().await.ok_or(Error::RecvError)
    }
}

impl Deref for Tui {
    type Target = ratatui::Terminal<Backend<std::io::Stdout>>;

    fn deref(&self) -> &Self::Target {
        &self.terminal
    }
}

impl DerefMut for Tui {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.terminal
    }
}

impl Drop for Tui {
    fn drop(&mut self) {
        self.cancel();
    }
}
