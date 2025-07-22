use crate::error::Error;
use std::{
    ops::{Deref, DerefMut},
    time::Duration,
};

use crossterm::{
    cursor,
    event::{
        EnableBracketedPaste, EnableMouseCapture, Event as CrosstermEvent,
        KeyEvent, KeyEventKind, MouseEvent,
    },
    terminal::EnterAlternateScreen,
};
use futures::{FutureExt, StreamExt};
use ratatui::backend::CrosstermBackend as Backend;
use serde::{Deserialize, Serialize};
use tokio::{
    sync::mpsc::{self, Receiver, Sender},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;
use tracing::error;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Event {
    Init,
    Quit,
    Error,
    Closed,
    Tick,
    Render,
    FocusGained,
    FocusLost,
    Paste(String),
    Key(KeyEvent),
    Mouse(MouseEvent),
    Resize(u16, u16),
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
        let _event_tx = self.event_tx.clone();

        tokio::spawn(async move {
            let mut reader = crossterm::event::EventStream::new();
            let mut tick_interval = tokio::time::interval(tick_delay);
            let mut render_interval = tokio::time::interval(render_delay);

            _event_tx.send(Event::Init).await.unwrap();

            loop {
                let tick_delay = tick_interval.tick();
                let render_delay = render_interval.tick();
                let crossterm_event = reader.next().fuse();

                tokio::select! {
                    _ = cancellation_token.cancelled() => {
                        break;
                    }
                    maybe_event = crossterm_event => {
                        match maybe_event {
                            Some(Ok(evt)) => {
                                match evt {
                                    CrosstermEvent::Key(key) => {
                                        if key.kind == KeyEventKind::Press {
                                            _event_tx.send(Event::Key(key)).await.unwrap();
                                        }
                                    },
                                    CrosstermEvent::Mouse(mouse) => {
                                        _event_tx.send(Event::Mouse(mouse)).await.unwrap();
                                    },
                                    CrosstermEvent::Resize(x, y) => {
                                        _event_tx.send(Event::Resize(x, y)).await.unwrap();
                                    },
                                    CrosstermEvent::FocusLost => {
                                        _event_tx.send(Event::FocusLost).await.unwrap();
                                    },
                                    CrosstermEvent::FocusGained => {
                                        _event_tx.send(Event::FocusGained).await.unwrap();
                                    },
                                    CrosstermEvent::Paste(s) => {
                                        _event_tx.send(Event::Paste(s)).await.unwrap();
                                    },
                                }
                            }
                            Some(Err(_)) => {
                                 _event_tx.send(Event::Error).await.unwrap();
                            }
                            None => {},
                        }
                    },
                    _ = tick_delay => {
                        _event_tx.send(Event::Tick).await.unwrap();
                    },
                    _ = render_delay => {
                        _event_tx.send(Event::Render).await.unwrap();
                    },
                }
            }
        });
    }

    pub fn stop(&self) {
        let mut counter = 0;
        self.cancel();

        while !self.task.is_finished() {
            std::thread::sleep(Duration::from_millis(1));
            counter += 1;
            if counter > 50 {
                self.task.abort();
            }
            if counter > 100 {
                error!(
                    "Failed to abort task in 100 milliseconds for unknown \
                     reason"
                );
                break;
            }
        }
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

    pub fn resume(&mut self) -> Result<(), Error> {
        self.run()?;
        Ok(())
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
