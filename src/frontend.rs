use actix::prelude::*;
use std::{
    io,
    time::{Duration, Instant},
};

use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use tui::{
    backend::CrosstermBackend,
    style::{Color, Style},
    Terminal,
};

use crate::{models::backend::BackendMessage, torrent_list::TorrentList};

#[derive(Clone)]
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

#[derive(Message)]
#[rtype(result = "()")]
pub enum FrontendMessage {
    Quit,
}

// actor
#[derive(Clone)]
pub struct Frontend {
    pub style: AppStyle,
    pub recipient: Recipient<BackendMessage>,
}

impl Actor for Frontend {
    type Context = Context<Self>;

    fn started(&mut self, ctx: &mut Self::Context) {
        self.run(ctx).unwrap();
    }

    fn stopping(&mut self, _ctx: &mut Self::Context) -> Running {
        let stdout = io::stdout();
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend).unwrap();
        disable_raw_mode().unwrap();
        execute!(
            terminal.backend_mut(),
            LeaveAlternateScreen,
            DisableMouseCapture
        )
        .unwrap();
        terminal.show_cursor().unwrap();
        self.recipient.do_send(BackendMessage::Quit);
        Running::Stop
    }
}

impl Frontend {
    pub fn new(recipient: Recipient<BackendMessage>) -> Result<Frontend, std::io::Error> {
        // setup terminal
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
        let style = AppStyle::new();

        Ok(Frontend { recipient, style })
    }

    pub fn run(&mut self, ctx: &mut Context<Frontend>) -> Result<(), std::io::Error> {
        let stdout = io::stdout();
        let mut last_tick = Instant::now();
        let tick_rate = Duration::from_millis(250);
        let timeout = Duration::from_secs(3);
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;
        let mut page = TorrentList::new();

        ctx.run_interval(tick_rate, move |_act, ctx| {
            if Instant::now().duration_since(last_tick) > timeout {
                ctx.stop();
            }

            terminal.draw(|f| page.draw(f)).unwrap();

            if let Event::Key(k) = event::read().unwrap() {
                page.keybindings(k.code, ctx);
            }

            last_tick = Instant::now();
        });

        Ok(())
    }
}

impl Handler<FrontendMessage> for Frontend {
    type Result = ();

    fn handle(&mut self, msg: FrontendMessage, _ctx: &mut Self::Context) -> Self::Result {
        match msg {
            FrontendMessage::Quit => {}
        }
    }
}
