use ratatui::{
    prelude::*,
    widgets::{Axis, Block, Borders, Chart, Dataset, GraphType},
    Frame,
};
use tokio::time::Instant;
use vincenzo::utils::to_human_readable;

use crate::PALETTE;

/// Last 60 seconds
const MAX_DATA_POINTS: usize = 60;
const TARGET_WINDOW: f64 = 60.0;

#[derive(Clone)]
pub struct NetworkChart {
    download_data: Vec<(f64, f64)>,
    max_download_rate: f64,
    start_time: Instant,
    current_time: f64,
}

impl Default for NetworkChart {
    fn default() -> Self {
        Self {
            download_data: Vec::with_capacity(MAX_DATA_POINTS),
            max_download_rate: 1.0,
            start_time: Instant::now(),
            current_time: 0.0,
        }
    }
}

impl NetworkChart {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn on_tick(&mut self, download_rate: f64) {
        self.current_time = self.start_time.elapsed().as_secs_f64();

        if download_rate > self.max_download_rate {
            self.max_download_rate = download_rate * 1.1
        }

        self.download_data.push((self.current_time, download_rate));

        // remove data points outside the time window
        let min_time = self.current_time - TARGET_WINDOW;
        self.download_data.retain(|&(t, _)| t >= min_time);
    }

    pub fn draw(&self, frame: &mut Frame, area: Rect) {
        if self.download_data.is_empty() {
            return;
        }

        // Calculate time window bounds
        let min_time = (self.current_time - TARGET_WINDOW).max(0.0);
        let max_time = min_time + TARGET_WINDOW;

        let name = format!(
            "Download {}",
            to_human_readable(self.download_data.last().unwrap().1)
        );

        let dataset = Dataset::default()
            .name(name)
            .marker(symbols::Marker::Braille)
            .graph_type(GraphType::Line)
            .style(PALETTE.primary)
            .data(&self.download_data);

        let x_labels = vec![
            Span::styled("60s", Style::default().add_modifier(Modifier::BOLD)),
            Span::styled("30s", Style::default().add_modifier(Modifier::BOLD)),
            Span::styled("0s", Style::default().add_modifier(Modifier::BOLD)),
        ];

        let y_labels = vec![
            Span::styled("0", Style::default().add_modifier(Modifier::BOLD)),
            format_bytes(self.max_download_rate / 3.0),
            format_bytes(self.max_download_rate / 2.0),
            format_bytes(self.max_download_rate),
        ];

        let chart = Chart::new(vec![dataset])
            .block(Block::default().title(" Network ").borders(Borders::ALL))
            .x_axis(
                Axis::default()
                    .title("Seconds")
                    .style(PALETTE.base_style)
                    .bounds([min_time, max_time])
                    .labels(x_labels),
            )
            .y_axis(
                Axis::default()
                    .title("Bytes")
                    .style(PALETTE.base_style)
                    .bounds([0.0, self.max_download_rate])
                    .labels(y_labels),
            );

        frame.render_widget(chart, area);
    }
}

fn format_bytes(bytes: f64) -> Span<'static> {
    Span::styled(
        to_human_readable(bytes),
        Style::default().add_modifier(Modifier::BOLD),
    )
}
