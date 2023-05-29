use actix::prelude::*;
use std::{io, time::Duration};

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

use crate::{backend::BackendMessage, torrent_list::TorrentList};

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

        // enable_raw_mode()?;
        // let mut stdout = io::stdout();
        // execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
        let style = AppStyle::new();

        Ok(Frontend { recipient, style })
    }

    fn run(&mut self, ctx: &mut Context<Frontend>) -> Result<(), std::io::Error> {
        let stdout = io::stdout();
        let tick_rate = Duration::from_millis(100);
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;
        let mut page = TorrentList::new();

        self.recipient.do_send(BackendMessage::AddMagnet(
            // "magnet:?xt=urn:btih:56BC861F42972DEA863AE853362A20E15C7BA07E&dn=Rust%20for%20Rustaceans%3A%20Idiomatic%20Programming&tr=udp%3A%2F%2Ftracker.coppersurfer.tk%3A6969%2Fannounce&tr=udp%3A%2F%2Ftracker.openbittorrent.com%3A6969%2Fannounce&tr=udp%3A%2F%2F9.rarbg.to%3A2710%2Fannounce&tr=udp%3A%2F%2F9.rarbg.me%3A2780%2Fannounce&tr=udp%3A%2F%2F9.rarbg.to%3A2730%2Fannounce&tr=udp%3A%2F%2Ftracker.opentrackr.org%3A1337&tr=http%3A%2F%2Fp4p.arenabg.com%3A1337%2Fannounce&tr=udp%3A%2F%2Ftracker.torrent.eu.org%3A451%2Fannounce&tr=udp%3A%2F%2Ftracker.tiny-vps.com%3A6969%2Fannounce&tr=udp%3A%2F%2Fopen.stealth.si%3A80%2Fannounce".to_owned()
            "magnet:?xt=urn:btih:48AAC768A865798307DDD4284BE77644368DD2C7&amp;dn=Kerkour%20S.%20Black%20Hat%20Rust...Rust%20programming%20language%202022&amp;tr=udp%3A%2F%2Ftracker.coppersurfer.tk%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.openbittorrent.com%3A6969%2Fannounce&amp;tr=udp%3A%2F%2F9.rarbg.to%3A2710%2Fannounce&amp;tr=udp%3A%2F%2F9.rarbg.me%3A2780%2Fannounce&amp;tr=udp%3A%2F%2F9.rarbg.to%3A2730%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.opentrackr.org%3A1337&amp;tr=http%3A%2F%2Fp4p.arenabg.com%3A1337%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.torrent.eu.org%3A451%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.tiny-vps.com%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Fopen.stealth.si%3A80%2Fannounce".to_owned()
        ));

        ctx.run_interval(tick_rate, move |act, ctx| {
            // dont draw for now
            // terminal.draw(|f| page.draw(f)).unwrap();

            if crossterm::event::poll(tick_rate).unwrap() {
                let k = event::read().unwrap();
                if let Event::Key(k) = k {
                    page.keybindings(k.code, ctx, act);
                }
            }
        });

        Ok(())
    }
}

impl Handler<FrontendMessage> for Frontend {
    type Result = ();

    fn handle(&mut self, msg: FrontendMessage, _ctx: &mut Self::Context) -> Self::Result {
        match msg {
            FrontendMessage::Quit => {
                println!("backend called quit on frontend!");
            }
        }
    }
}
