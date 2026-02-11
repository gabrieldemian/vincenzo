use crate::error::Error;
use crossterm::event::{DisableMouseCapture, EventStream};
use futures::{FutureExt, StreamExt};
use ratatui::{
    Terminal,
    backend::{Backend, CrosstermBackend},
    crossterm::{
        self,
        event::EnableMouseCapture,
        terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
    },
};
use std::{io, time::Duration};
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

pub struct Tui<B: Backend> {
    pub terminal: Terminal<B>,
    pub task: JoinHandle<()>,
    pub cancellation_token: CancellationToken,
    pub event_rx: Receiver<Event>,
    pub event_tx: Sender<Event>,
}

impl<B> Tui<B>
where
    B: Backend,
    Error: From<B::Error>,
{
    const FRAME_RATE: f64 = 60.0;
    const TICK_RATE: f64 = 4.0;

    pub fn new(terminal: Terminal<B>) -> Result<Self, Error> {
        let (event_tx, event_rx) = mpsc::channel(50);
        let cancellation_token = CancellationToken::new();
        let task = tokio::spawn(async {});
        Ok(Self { terminal, task, cancellation_token, event_rx, event_tx })
    }

    pub fn init(&mut self) -> Result<(), Error> {
        terminal::enable_raw_mode().unwrap();
        ratatui::crossterm::execute!(
            io::stdout(),
            EnterAlternateScreen,
            EnableMouseCapture
        )?;
        std::panic::set_hook(Box::new(move |_panic| {
            Self::reset().expect("failed to reset the terminal");
            std::process::exit(1);
        }));

        let tick_delay = Duration::from_secs_f64(1.0 / Self::TICK_RATE);
        let render_delay = Duration::from_secs_f64(1.0 / Self::FRAME_RATE);

        self.cancellation_token = CancellationToken::new();

        let cancellation_token = self.cancellation_token.clone();
        let event_tx = self.event_tx.clone();

        tokio::spawn(async move {
            let mut reader = EventStream::default();
            let mut tick_interval = tokio::time::interval(tick_delay);
            let mut render_interval = tokio::time::interval(render_delay);

            loop {
                let tick_delay = tick_interval.tick();
                let render_delay = render_interval.tick();

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

        Ok(())
    }

    /// Reset the terminal interface.
    /// It disables the raw mode and reverts back the terminal properties.
    pub fn reset() -> Result<(), Error> {
        terminal::disable_raw_mode()?;
        ratatui::crossterm::execute!(
            io::stdout(),
            LeaveAlternateScreen,
            DisableMouseCapture
        )?;
        Terminal::new(CrosstermBackend::new(io::stdout()))?.show_cursor()?;
        Ok(())
    }

    /// Exits the terminal interface.
    /// It disables the raw mode and reverts back the terminal properties.
    pub fn exit(&mut self) -> Result<(), Error> {
        terminal::disable_raw_mode()?;
        ratatui::crossterm::execute!(
            io::stdout(),
            LeaveAlternateScreen,
            DisableMouseCapture
        )?;
        self.terminal.show_cursor()?;
        Ok(())
    }

    pub fn cancel(&mut self) -> Result<(), Error> {
        self.cancellation_token.cancel();
        self.exit()
    }

    pub async fn next(&mut self) -> Result<Event, Error> {
        self.event_rx.recv().await.ok_or(Error::RecvError)
    }
}
