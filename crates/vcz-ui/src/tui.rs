use crate::{action::Action, error::Error};
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
use signal_hook::consts::signal::*;
use signal_hook_tokio::Signals;
use std::{io, time::Duration};
use tokio::{
    sync::mpsc::{self, Receiver, Sender},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

pub struct Tui<B: Backend> {
    pub terminal: Terminal<B>,
    pub task: JoinHandle<()>,
    pub cancellation_token: CancellationToken,
    pub rx: Receiver<Action>,
    pub tx: Sender<Action>,
}

impl<B> Tui<B>
where
    B: Backend + 'static,
    Error: From<B::Error>,
{
    const FRAME_RATE: f64 = 60.0;

    pub fn new(terminal: Terminal<B>) -> Result<Self, Error> {
        let (tx, rx) = mpsc::channel(64);
        let cancellation_token = CancellationToken::new();
        let task = tokio::spawn(async {});
        Ok(Self { terminal, task, cancellation_token, rx, tx })
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

        let signals = Signals::new([SIGHUP, SIGTERM, SIGINT, SIGQUIT])?;
        let handle = signals.handle();
        let _tx = self.tx.clone();
        let signals_task = tokio::spawn(Self::handle_signals(signals, _tx));

        let render_delay = Duration::from_secs_f64(1.0 / Self::FRAME_RATE);

        self.cancellation_token = CancellationToken::new();

        let cancellation_token = self.cancellation_token.clone();
        let tx = self.tx.clone();

        tokio::spawn(async move {
            let mut reader = EventStream::default();
            let mut render_interval = tokio::time::interval(render_delay);

            loop {
                let render_delay = render_interval.tick();
                tokio::select! {
                    event = reader.next().fuse() => {
                        if let Some(Ok(event)) = event {
                            tx.send(Action::Input(event.into())).await?;
                        }
                    },
                    _ = render_delay => {
                        tx.send(Action::Render).await?;
                    },
                    _ = cancellation_token.cancelled() => {
                        signals_task.abort();
                        handle.close();
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

    pub async fn next(&mut self) -> Result<Action, Error> {
        self.rx.recv().await.ok_or(Error::RecvError)
    }

    async fn handle_signals(mut signals: Signals, tx: Sender<Action>) {
        while let Some(signal) = signals.next().await {
            match signal {
                SIGHUP => {
                    // Reload configuration
                    // Reopen the log file
                }
                _sig @ (SIGTERM | SIGINT | SIGQUIT | SIGKILL) => {
                    let _ = tx.send(Action::Quit).await;
                }
                _ => unreachable!(),
            }
        }
    }
}
