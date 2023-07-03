use std::{io, time::Duration};
use tracing::info;

use crossterm::{
    event::DisableMouseCapture,
    execute,
    terminal::{disable_raw_mode, LeaveAlternateScreen},
};
use tui::{
    backend::CrosstermBackend,
    style::{Color, Style},
    Terminal,
};

use crate::torrent_list::TorrentList;

#[derive(Clone)]
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

pub enum FrontendMessage {
    Quit,
}

// actor
#[derive(Clone)]
pub struct Frontend {
    pub style: AppStyle,
    // pub recipient: Recipient<BackendMessage>,
}

impl Frontend {
    pub fn new() -> Result<Frontend, std::io::Error> {
        // setup terminal

        // enable_raw_mode()?;
        // let mut stdout = io::stdout();
        // execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
        let style = AppStyle::new();

        Ok(Frontend {
            // recipient,
            style,
        })
    }

    fn _stop(&mut self) -> Result<(), std::io::Error> {
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
        // self.recipient.do_send(BackendMessage::Quit);
        Ok(())
    }

    fn _handle(&mut self, msg: FrontendMessage) {
        match msg {
            FrontendMessage::Quit => {
                info!("backend called quit on frontend!");
            }
        }
    }

    fn _run(&mut self) -> Result<(), std::io::Error> {
        let stdout = io::stdout();
        let _tick_rate = Duration::from_millis(100);
        let backend = CrosstermBackend::new(stdout);
        let _terminal = Terminal::new(backend)?;
        let _page = TorrentList::new();

        // self.recipient_async.do_send(AddMagnetMessage{link:
        //     "magnet:?xt=urn:btih:48AAC768A865798307DDD4284BE77644368DD2C7&amp;dn=Kerkour%20S.%20Black%20Hat%20Rust...Rust%20programming%20language%202022&amp;tr=udp%3A%2F%2Ftracker.coppersurfer.tk%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.openbittorrent.com%3A6969%2Fannounce&amp;tr=udp%3A%2F%2F9.rarbg.to%3A2710%2Fannounce&amp;tr=udp%3A%2F%2F9.rarbg.me%3A2780%2Fannounce&amp;tr=udp%3A%2F%2F9.rarbg.to%3A2730%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.opentrackr.org%3A1337&amp;tr=http%3A%2F%2Fp4p.arenabg.com%3A1337%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.torrent.eu.org%3A451%2Fannounce&amp;tr=udp%3A%2F%2Ftracker.tiny-vps.com%3A6969%2Fannounce&amp;tr=udp%3A%2F%2Fopen.stealth.si%3A80%2Fannounce".to_owned()
        // });
        //
        // ctx.run_interval(tick_rate, move |act, ctx| {
        //     // dont draw for now
        //     // terminal.draw(|f| page.draw(f)).unwrap();
        //
        //     if crossterm::event::poll(tick_rate).unwrap() {
        //         let k = event::read().unwrap();
        //         if let Event::Key(k) = k {
        //             page.keybindings(k.code, ctx, act);
        //         }
        //     }
        // });

        Ok(())
    }
}
