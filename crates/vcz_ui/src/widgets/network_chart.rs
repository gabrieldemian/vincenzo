use ratatui::{
    Frame,
    prelude::*,
    widgets::{
        Axis, Block, Borders, Chart, Dataset, GraphType, LegendPosition,
    },
};
use tokio::time::Instant;
use vincenzo::{torrent::InfoHash, utils::to_human_readable};

use crate::PALETTE;

/// Last 60 seconds
const MAX_DATA_POINTS: usize = 60;
const TARGET_WINDOW: f64 = 60.0;
const SMOOTHING_FACTOR: f64 = 0.05; // 5%

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

        self.download_data.push((self.current_time, download_rate));
        self.upload_data.push((self.current_time, upload_rate));

        let min_time = self.current_time - TARGET_WINDOW;
        self.download_data.retain(|&(t, _)| t >= min_time);
        self.upload_data.retain(|&(t, _)| t >= min_time);

        // calculate the maximum of both datasets using log scale
        let current_max_log = self
            .download_data
            .iter()
            .chain(self.upload_data.iter())
            .map(|&(_, rate)| (rate + 1.0).log10()) // add 1 to avoid log(0)
            .fold(0.0, f64::max);

        // if we have a new maximum, update immediately (no smoothing)
        if self.max_rate < current_max_log {
            self.max_rate = current_max_log * 1.1;
        } else {
            self.max_rate = SMOOTHING_FACTOR * current_max_log
                + (1.0 - SMOOTHING_FACTOR) * self.max_rate;
        }

        self.max_rate = self.max_rate.max(0.1);
    }

    pub fn draw(&self, frame: &mut Frame, area: Rect, dim: bool) {
        if self.download_data.is_empty() || self.upload_data.is_empty() {
            return;
        }

        let min_time = (self.current_time - TARGET_WINDOW).max(0.0);
        let max_time = min_time + TARGET_WINDOW;

        let log_download_data: Vec<(f64, f64)> = self
            .download_data
            .iter()
            .map(|&(t, rate)| (t, (rate + 1.0).log10()))
            .collect();

        let log_upload_data: Vec<(f64, f64)> = self
            .upload_data
            .iter()
            .map(|&(t, rate)| (t, (rate + 1.0).log10()))
            .collect();

        let download_dataset = Dataset::default()
            .name("")
            .marker(symbols::Marker::Braille)
            .graph_type(GraphType::Line)
            .style(PALETTE.blue)
            .data(&log_download_data);

        let upload_dataset = Dataset::default()
            .name("")
            .marker(symbols::Marker::Braille)
            .graph_type(GraphType::Line)
            .style(PALETTE.green)
            .data(&log_upload_data);

        let x_labels = vec![
            Span::from("0s").bold(),
            Span::from("30s").bold(),
            Span::from("60s").bold(),
        ];

        let y_ticks = [
            0.0,                  // 10^0 = 1 B/s
            self.max_rate * 0.25, // 25% of max
            self.max_rate * 0.5,  // 50% of max
            self.max_rate * 0.75, // 75% of max
            self.max_rate,        // 100% of max
        ];

        let y_labels: Vec<Span> = y_ticks
            .iter()
            .map(|&log_value| {
                let value = 10_f64.powf(log_value);
                Span::styled(
                    to_human_readable(value),
                    Style::default().add_modifier(Modifier::BOLD),
                )
            })
            .collect();

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

        let mut chart = Chart::new(vec![download_dataset, upload_dataset])
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

        if dim {
            chart = chart.dim();
        }

        frame.render_widget(chart, area);
    }
}
