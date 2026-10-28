//! Die letzten Log-Zeilen im Speicher – für die Log-Ansicht im Browser.

use std::{
    collections::VecDeque,
    fmt::Write as _,
    sync::{Arc, Mutex},
};

use serde::Serialize;
use tracing::{
    Event, Subscriber,
    field::{Field, Visit},
};
use tracing_subscriber::{Layer, layer::Context};

const CAPACITY: usize = 1000;

#[derive(Debug, Clone, Serialize)]
pub struct LogLine {
    pub seq: u64,
    /// Unix-Zeit in Millisekunden.
    pub ts: u128,
    pub level: String,
    pub target: String,
    pub message: String,
}

#[derive(Default)]
struct Inner {
    lines: VecDeque<LogLine>,
    next_seq: u64,
}

#[derive(Clone, Default)]
pub struct LogBuffer {
    inner: Arc<Mutex<Inner>>,
}

impl LogBuffer {
    /// Zeilen mit `seq > after` (höchstens `limit`).
    pub fn since(&self, after: u64, limit: usize) -> Vec<LogLine> {
        let Ok(inner) = self.inner.lock() else {
            return Vec::new();
        };
        let mut out: Vec<_> = inner
            .lines
            .iter()
            .filter(|l| l.seq > after)
            .cloned()
            .collect();
        if out.len() > limit {
            out.drain(..out.len() - limit);
        }
        out
    }

    fn push(&self, mut line: LogLine) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        inner.next_seq += 1;
        line.seq = inner.next_seq;
        if inner.lines.len() >= CAPACITY {
            inner.lines.pop_front();
        }
        inner.lines.push_back(line);
    }

    /// `tracing`-Layer, der in diesen Puffer schreibt.
    pub fn layer(&self) -> LogBufferLayer {
        LogBufferLayer {
            buffer: self.clone(),
        }
    }
}

pub struct LogBufferLayer {
    buffer: LogBuffer,
}

#[derive(Default)]
struct MessageVisitor {
    message: String,
    fields: String,
}

impl Visit for MessageVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.message, "{value:?}");
        } else {
            let _ = write!(self.fields, " {}={value:?}", field.name());
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message.push_str(value);
        } else {
            let _ = write!(self.fields, " {}={value}", field.name());
        }
    }
}

impl<S: Subscriber> Layer<S> for LogBufferLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        self.buffer.push(LogLine {
            seq: 0,
            ts,
            level: event.metadata().level().to_string(),
            target: event.metadata().target().to_string(),
            message: format!("{}{}", visitor.message, visitor.fields),
        });
    }
}
