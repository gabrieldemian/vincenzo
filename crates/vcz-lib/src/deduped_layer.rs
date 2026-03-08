//! An implementation of [`tracing_subscriber::Layer`] that can mutate fields.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tracing::{
    Event, Level, Subscriber,
    field::{Field, Visit},
    level_filters::LevelFilter,
    span::{Attributes, Id},
};
use tracing_subscriber::{Layer, layer::Context};

#[derive(Default)]
struct SpanData {
    fields: HashMap<String, String>,
    order: Vec<String>,
}

struct FieldVisitor<'a>(&'a mut HashMap<String, String>);

impl<'a> Visit for FieldVisitor<'a> {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.0.insert(field.name().to_string(), format!("{value:?}"));
    }
}

struct SpanCreationVisitor<'a>(
    &'a mut Vec<String>,
    &'a mut HashMap<String, String>,
);

impl<'a> Visit for SpanCreationVisitor<'a> {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let name = field.name().to_string();
        self.0.push(name.clone());
        self.1.insert(name, format!("{value:?}"));
    }
}

struct EventVisitor {
    fields: Vec<(String, String)>,
    message: Option<String>,
}

impl EventVisitor {
    fn new() -> Self {
        Self { fields: Vec::new(), message: None }
    }
}

impl Visit for EventVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let name = field.name();
        if name == "message" {
            self.message = Some(format!("{value:?}"));
        } else {
            self.fields.push((name.to_string(), format!("{value:?}")));
        }
    }
}

pub struct MutableLayer {
    spans: Arc<Mutex<HashMap<Id, SpanData>>>,
    max_level: LevelFilter,
}

impl MutableLayer {
    pub fn new() -> Self {
        Self {
            spans: Arc::new(Mutex::new(HashMap::new())),
            max_level: LevelFilter::INFO,
        }
    }

    pub fn with_max_level(mut self, level: LevelFilter) -> Self {
        self.max_level = level;
        self
    }
}

impl Default for MutableLayer {
    fn default() -> Self {
        Self::new()
    }
}

impl<S: Subscriber> Layer<S> for MutableLayer {
    fn on_new_span(
        &self,
        attrs: &Attributes<'_>,
        id: &Id,
        _ctx: Context<'_, S>,
    ) {
        let mut data = SpanData::default();
        let mut visitor =
            SpanCreationVisitor(&mut data.order, &mut data.fields);
        attrs.record(&mut visitor);
        self.spans.lock().unwrap().insert(id.clone(), data);
    }

    fn on_record(
        &self,
        id: &Id,
        values: &tracing::span::Record<'_>,
        _ctx: Context<'_, S>,
    ) {
        if let Some(data) = self.spans.lock().unwrap().get_mut(id) {
            let mut visitor = FieldVisitor(&mut data.fields);
            values.record(&mut visitor);
        }
    }

    fn on_close(&self, id: Id, _ctx: Context<'_, S>) {
        self.spans.lock().unwrap().remove(&id);
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        if event.metadata().level() > &self.max_level {
            return;
        }

        let current_span = ctx.current_span();
        let span_data = current_span.id().and_then(|id| {
            self.spans
                .lock()
                .unwrap()
                .get(id)
                .map(|d| (d.order.clone(), d.fields.clone()))
        });

        let mut event_visitor = EventVisitor::new();
        event.record(&mut event_visitor);
        let event_fields = event_visitor.fields;
        let message = event_visitor.message.unwrap_or_default();

        let mut final_fields = Vec::new();

        if let Some((span_order, span_fields)) = span_data {
            let event_map: HashMap<&str, &str> = event_fields
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect();

            for name in &span_order {
                if let Some(value) = event_map.get(name.as_str()) {
                    final_fields.push((name.clone(), (*value).to_string()));
                } else if let Some(value) = span_fields.get(name) {
                    final_fields.push((name.clone(), value.clone()));
                }
            }

            for (name, value) in event_fields {
                if !span_order.contains(&name) {
                    final_fields.push((name, value));
                }
            }
        } else {
            final_fields = event_fields;
        }

        let fields_str = if final_fields.is_empty() {
            String::new()
        } else {
            let parts: Vec<_> = final_fields
                .into_iter()
                .map(|(k, v)| format!("{}=\"{}\"", k, v))
                .collect();
            format!(" {}", parts.join(" "))
        };

        let level = event.metadata().level();
        let (level_color, level_str) = match *level {
            Level::ERROR => (RED, "ERROR"),
            Level::WARN => (YELLOW, "WARN"),
            Level::INFO => (GREEN, "INFO"),
            Level::DEBUG => (BLUE, "DEBUG"),
            Level::TRACE => (MAGENTA, "TRACE"),
        };

        let span_name = current_span.metadata().map(|m| m.name()).unwrap_or("");
        let name_part = if span_name.is_empty() {
            String::new()
        } else {
            format!(" \x1b[1m{}:\x1b[22m", span_name)
        };

        let line = format!(
            "{level_color}{level_str}{RESET}{name_part}{fields_str} {message}"
        );
        println!("{line}");
    }
}

const RED: &str = "\x1b[31m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const BLUE: &str = "\x1b[34m";
const MAGENTA: &str = "\x1b[35m";
const RESET: &str = "\x1b[0m";
