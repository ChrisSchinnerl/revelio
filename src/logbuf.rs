//! In-app log capture: a [`log::Log`] implementation that writes through to
//! env_logger (stdout) and keeps the most recent records for the Logs panel.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use log::{Level, LevelFilter, Log, Metadata, Record};
use sia_storage::Utc;

/// How many records to keep; the oldest are dropped as new ones arrive.
const CAPACITY: usize = 2000;

/// One captured log record.
#[derive(Clone)]
pub struct LogLine {
    /// UTC wall-clock time, `HH:MM:SS.mmm` (matching env_logger's stdout).
    pub time: String,
    pub level: Level,
    pub target: String,
    pub message: String,
}

/// A bounded, shared tail of the most recent log records.
#[derive(Clone, Default)]
pub struct LogBuffer(Arc<Mutex<VecDeque<LogLine>>>);

impl LogBuffer {
    fn push(&self, line: LogLine) {
        let mut lines = self.0.lock().unwrap();
        if lines.len() == CAPACITY {
            lines.pop_front();
        }
        lines.push_back(line);
    }

    /// The records at or above `level` whose target or message contains
    /// `query` (case-insensitive), oldest first, and how many are held in
    /// total. Filtering happens under the lock so only matches are cloned.
    pub fn snapshot(&self, level: LevelFilter, query: &str) -> (Vec<LogLine>, usize) {
        let query = query.trim().to_lowercase();
        let lines = self.0.lock().unwrap();
        let matches = lines
            .iter()
            .filter(|l| l.level <= level)
            .filter(|l| {
                query.is_empty()
                    || l.message.to_lowercase().contains(&query)
                    || l.target.to_lowercase().contains(&query)
            })
            .cloned()
            .collect();
        (matches, lines.len())
    }

    pub fn clear(&self) {
        self.0.lock().unwrap().clear();
    }
}

/// Captures records into a [`LogBuffer`] on their way to env_logger.
struct Logger {
    inner: env_logger::Logger,
    buf: LogBuffer,
}

impl Log for Logger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        self.inner.enabled(metadata)
    }

    fn log(&self, record: &Record) {
        // Checked here too, so records env_logger filters out aren't buffered.
        if !self.inner.matches(record) {
            return;
        }
        self.inner.log(record);
        self.buf.push(LogLine {
            time: Utc::now().format("%H:%M:%S%.3f").to_string(),
            level: record.level(),
            target: record.target().to_string(),
            message: record.args().to_string(),
        });
    }

    fn flush(&self) {
        self.inner.flush();
    }
}

/// Installs the logger and returns the buffer the Logs panel reads from.
/// `RUST_LOG` still wins; the default captures revelio's own debug records so
/// the panel shows every operation out of the box.
pub fn init() -> LogBuffer {
    let inner = env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("info,revelio=debug"),
    )
    .target(env_logger::Target::Stdout)
    .build();
    let buf = LogBuffer::default();
    log::set_max_level(inner.filter());
    let logger = Logger {
        inner,
        buf: buf.clone(),
    };
    if log::set_boxed_logger(Box::new(logger)).is_err() {
        eprintln!("a logger is already installed; in-app logs will be empty");
    }
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(level: Level, target: &str, message: &str) -> LogLine {
        LogLine {
            time: "00:00:00.000".into(),
            level,
            target: target.into(),
            message: message.into(),
        }
    }

    #[test]
    fn test_snapshot_filters_by_level_and_query() {
        let buf = LogBuffer::default();
        buf.push(line(Level::Error, "revelio::backend", "sync failed"));
        buf.push(line(Level::Info, "revelio::app", "using database"));
        buf.push(line(Level::Debug, "sia_storage", "refreshed hosts"));

        // A level filter keeps everything at or above it.
        let (lines, total) = buf.snapshot(LevelFilter::Info, "");
        assert_eq!(total, 3);
        assert_eq!(lines.len(), 2);
        assert!(lines.iter().all(|l| l.level <= Level::Info));

        // The query matches the message or the target, case-insensitively.
        let (lines, _) = buf.snapshot(LevelFilter::Trace, "FAILED");
        assert_eq!(lines.len(), 1);
        let (lines, _) = buf.snapshot(LevelFilter::Trace, "sia_storage");
        assert_eq!(lines.len(), 1);
        let (lines, _) = buf.snapshot(LevelFilter::Error, "sia_storage");
        assert!(lines.is_empty());
    }

    #[test]
    fn test_buffer_drops_oldest_over_capacity() {
        let buf = LogBuffer::default();
        for i in 0..CAPACITY + 10 {
            buf.push(line(Level::Info, "test", &i.to_string()));
        }
        let (lines, total) = buf.snapshot(LevelFilter::Trace, "");
        assert_eq!(total, CAPACITY);
        assert_eq!(lines.first().unwrap().message, "10");
        assert_eq!(lines.last().unwrap().message, (CAPACITY + 9).to_string());
    }
}
