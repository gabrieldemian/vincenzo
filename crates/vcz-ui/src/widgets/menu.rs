use crate::{
    PALETTE,
    action::{Action, Page},
    app::State,
};
use ratatui::{layout::Flex, prelude::*, widgets::Tabs};
use tokio::sync::mpsc::UnboundedSender;
use vcz_lib::VERSION;

static TITLES_LEN: isize = TITLES.len() as isize;
static TITLES: [&str; 2] = [
    " Torrents ",
    // " Logs ",
    " Info ",
];

#[derive(Clone, Debug)]
pub struct Menu {
    index: usize,
    tx: UnboundedSender<Action>,
}

impl Menu {
    pub fn new(tx: UnboundedSender<Action>) -> Self {
        Self { tx, index: 0 }
    }

    pub fn draw(&mut self, f: &mut Frame, area: Rect, state: &mut State) {
        let mut tag = Span::raw(format!("[ VCZ - v{VERSION} ]"))
            .style(PALETTE.highlight_fg);
        let chunks = Layout::horizontal([
            Constraint::Percentage(100),
            Constraint::Min(16),
        ])
        .flex(Flex::SpaceBetween)
        .split(area);

        let mut tabs = Tabs::new(TITLES)
            .highlight_style(PALETTE.highlight_bg.bold())
            .select(self.index)
            .padding("", "")
            .divider(" ");

        if state.should_dim {
            tabs = tabs.dim();
            tag = tag.dim();
        }

        f.render_widget(tabs, chunks[0]);
        f.render_widget(tag, chunks[1]);
    }

    #[inline]
    fn select_relative(&mut self, offset: isize) {
        self.index =
            (self.index as isize + offset).rem_euclid(TITLES_LEN) as usize;
    }

    #[inline]
    fn update_page(&self) {
        match self.index {
            0 => {
                let _ = self.tx.send(Action::ChangePage(Page::TorrentList));
            }
            1 => {
                let _ = self.tx.send(Action::ChangePage(Page::Info));
            }
            _ => {}
        }
    }

    #[inline]
    pub fn next(&mut self) {
        self.select_relative(1);
        self.update_page();
    }

    #[inline]
    pub fn previous(&mut self) {
        self.select_relative(-1);
        self.update_page();
    }
}
