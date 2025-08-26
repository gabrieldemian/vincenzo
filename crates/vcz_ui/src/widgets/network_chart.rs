use ratatui::{
    prelude::*,
    widgets::{
        Axis, Block, Borders, Chart, Dataset, GraphType, LegendPosition,
    },
    Frame,
};
use tokio::time::Instant;
use vincenzo::{torrent::InfoHash, utils::to_human_readable};

use crate::PALETTE;

/// Last 60 seconds
const MAX_DATA_POINTS: usize = 60;
const TARGET_WINDOW: f64 = 60.0;

#[derive(Clone)]
pub struct NetworkChart {
    pub info_hash: InfoHash,
    download_data: Vec<(f64, f64)>,
    upload_data: Vec<(f64, f64)>,
    max_rate: f64,
    start_time: Instant,
    current_time: f64,
}

impl Default for NetworkChart {
    fn default() -> Self {
        Self {
            info_hash: InfoHash::default(),
            download_data: Vec::with_capacity(MAX_DATA_POINTS),
            upload_data: Vec::with_capacity(MAX_DATA_POINTS),
            max_rate: 1.0,
            start_time: Instant::now(),
            current_time: 0.0,
        }
    }
}

impl NetworkChart {
    pub fn new(info_hash: InfoHash) -> Self {
        Self { info_hash, ..Default::default() }
    }

    pub fn on_tick(&mut self, download_rate: f64, upload_rate: f64) {
        self.current_time = self.start_time.elapsed().as_secs_f64();

        self.max_rate = self
            .max_rate
            .max(download_rate * 1.1) // Add 10% padding
            .max(upload_rate * 1.1);

        self.download_data.push((self.current_time, download_rate));
        self.upload_data.push((self.current_time, upload_rate));

        // remove data points outside the time window
        let min_time = self.current_time - TARGET_WINDOW;
        self.download_data.retain(|&(t, _)| t >= min_time);
        self.upload_data.retain(|&(t, _)| t >= min_time);

        // periodically reset max rate if it's much higher than current rates
        if (self.current_time as u64).is_multiple_of(10) {
            let current_max = self
                .download_data
                .iter()
                .chain(&self.upload_data)
                .map(|&(_, rate)| rate)
                .fold(0.0, f64::max);

            if self.max_rate > current_max * 2.0 {
                self.max_rate = current_max * 1.1;
            }
        }
    }

    pub fn draw(&self, frame: &mut Frame, area: Rect) {
        if self.download_data.is_empty() || self.upload_data.is_empty() {
            return;
        }

        // Calculate time window bounds
        let min_time = (self.current_time - TARGET_WINDOW).max(0.0);
        let max_time = min_time + TARGET_WINDOW;

        let download_dataset = Dataset::default()
            .name("")
            .marker(symbols::Marker::Braille)
            .graph_type(GraphType::Line)
            .style(PALETTE.blue)
            .data(&self.download_data);

        let upload_dataset = Dataset::default()
            .name("")
            .marker(symbols::Marker::Braille)
            .graph_type(GraphType::Line)
            .style(PALETTE.green)
            .data(&self.upload_data);

        let x_labels = vec![
            Span::from("0s").bold(),
            Span::from("30s").bold(),
            Span::from("60s").bold(),
        ];

        let y_labels = vec![
            Span::from("0").bold(),
            format_bytes(self.max_rate / 3.0),
            format_bytes(self.max_rate / 2.0),
            format_bytes(self.max_rate),
        ];

        let legend = vec![
            Span::styled(" ↓ ", PALETTE.blue),
            Span::styled(
                to_human_readable(self.download_data.last().unwrap().1),
                Into::<Style>::into(PALETTE.blue).bold(),
            ),
            Span::raw(" "),
            Span::styled(" ↑ ", PALETTE.green),
            Span::styled(
                to_human_readable(self.upload_data.last().unwrap().1),
                Into::<Style>::into(PALETTE.green).bold(),
            ),
            Span::raw(" "),
        ];

        let chart = Chart::new(vec![download_dataset, upload_dataset])
            .legend_position(Some(LegendPosition::TopRight))
            .hidden_legend_constraints((
                Constraint::Ratio(1, 2),
                Constraint::Ratio(1, 2),
            ))
            .block(
                Block::default()
                    .title(" Network ")
                    .title_bottom(Line::from(legend))
                    .borders(Borders::ALL),
            )
            .x_axis(
                Axis::default()
                    .style(PALETTE.base_style)
                    .bounds([min_time, max_time])
                    .labels(x_labels),
            )
            .y_axis(
                Axis::default()
                    .style(PALETTE.base_style)
                    .bounds([0.0, self.max_rate])
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
