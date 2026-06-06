//! Process-wide bounded capture of `log` records for the Logs tab.
//!
//! [`CaptureLogger`] is installed as the global logger at startup. Every
//! enabled record is teed to stderr (preserving the previous `env_logger`
//! console behavior) and into a fixed-capacity ring buffer that the UI
//! snapshots when the port detail panel's Logs tab refreshes.

use log::{LevelFilter, Log, Metadata, Record};
use std::collections::VecDeque;
use std::sync::Mutex;

/// Maximum number of captured lines kept in memory.
const CAPACITY: usize = 500;

/// A bounded FIFO of formatted log lines. Kept as a plain struct (capacity is
/// a field, not the global constant) so the eviction logic is unit-testable.
struct Ring {
    lines: VecDeque<String>,
    capacity: usize,
}

impl Ring {
    const fn new(capacity: usize) -> Self {
        Ring {
            lines: VecDeque::new(),
            capacity,
        }
    }

    /// Appends a line, evicting the oldest when full.
    fn push(&mut self, line: String) {
        if self.lines.len() == self.capacity {
            self.lines.pop_front();
        }
        self.lines.push_back(line);
    }

    /// Returns the captured lines, oldest first.
    fn snapshot(&self) -> Vec<String> {
        self.lines.iter().cloned().collect()
    }
}

static RING: Mutex<Ring> = Mutex::new(Ring::new(CAPACITY));

/// Appends a line to the process-wide ring buffer.
pub fn push(line: String) {
    RING.lock().unwrap_or_else(|e| e.into_inner()).push(line);
}

/// Snapshots the process-wide ring buffer, oldest line first.
pub fn snapshot() -> Vec<String> {
    RING.lock().unwrap_or_else(|e| e.into_inner()).snapshot()
}

/// Global logger that filters like `env_logger` (longest-prefix `target=level`
/// directives from `RUST_LOG` or a default spec) and tees each enabled record
/// to stderr and the ring buffer.
pub struct CaptureLogger {
    /// `(target prefix, max level)` directives, e.g. `("tunnels", Info)`.
    directives: Vec<(String, LevelFilter)>,
    /// Level for targets matching no directive (env_logger's default: Error).
    default_level: LevelFilter,
}

impl CaptureLogger {
    /// Builds the filter from `RUST_LOG`, falling back to `default_spec`
    /// (e.g. `"devtunnel_gui=debug,tunnels=info"`).
    pub fn from_env(default_spec: &str) -> Self {
        let spec = std::env::var("RUST_LOG").unwrap_or_else(|_| default_spec.to_string());
        Self::parse(&spec)
    }

    /// Parses a comma-separated filter spec: `target=level` directives plus an
    /// optional bare `level` that becomes the default. Invalid parts are skipped.
    fn parse(spec: &str) -> Self {
        let mut directives = Vec::new();
        let mut default_level = LevelFilter::Error;
        for part in spec.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            match part.split_once('=') {
                Some((target, level)) => {
                    if let Ok(l) = level.trim().parse() {
                        directives.push((target.trim().to_string(), l));
                    }
                }
                None => {
                    if let Ok(l) = part.parse() {
                        default_level = l;
                    }
                }
            }
        }
        CaptureLogger {
            directives,
            default_level,
        }
    }

    /// Installs `self` as the global logger and sets the max level to the most
    /// verbose directive. Errors only if a logger is already installed.
    pub fn install(self) -> Result<(), log::SetLoggerError> {
        let max = self
            .directives
            .iter()
            .map(|(_, l)| *l)
            .chain([self.default_level])
            .max()
            .unwrap_or(LevelFilter::Error);
        log::set_max_level(max);
        log::set_boxed_logger(Box::new(self))
    }

    /// Effective max level for `target`: the longest matching directive prefix
    /// wins (module-path boundary respected), else the default level.
    fn level_for(&self, target: &str) -> LevelFilter {
        self.directives
            .iter()
            .filter(|(t, _)| {
                target == t
                    || (target.starts_with(t.as_str()) && target[t.len()..].starts_with("::"))
            })
            .max_by_key(|(t, _)| t.len())
            .map(|(_, l)| *l)
            .unwrap_or(self.default_level)
    }
}

impl Log for CaptureLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= self.level_for(metadata.target())
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        // Technical/diagnostic content — intentionally not localized.
        let line = format!(
            "{:<5} {} — {}",
            record.level(),
            record.target(),
            record.args()
        );
        eprintln!("{line}");
        push(line);
    }

    fn flush(&self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_evicts_oldest_when_full() {
        let mut ring = Ring::new(3);
        for i in 0..5 {
            ring.push(format!("line {i}"));
        }
        assert_eq!(ring.snapshot(), vec!["line 2", "line 3", "line 4"]);
    }

    #[test]
    fn ring_snapshot_preserves_insertion_order() {
        let mut ring = Ring::new(10);
        ring.push("a".into());
        ring.push("b".into());
        assert_eq!(ring.snapshot(), vec!["a", "b"]);
    }

    #[test]
    fn global_push_and_snapshot_roundtrip() {
        push("hello from test".into());
        assert!(snapshot().iter().any(|l| l == "hello from test"));
    }

    #[test]
    fn filter_directives_apply_longest_prefix() {
        let logger = CaptureLogger::parse("devtunnel_gui=debug,tunnels=info,tunnels::ssh=trace");
        assert_eq!(logger.level_for("devtunnel_gui"), LevelFilter::Debug);
        assert_eq!(logger.level_for("devtunnel_gui::host"), LevelFilter::Debug);
        assert_eq!(logger.level_for("tunnels"), LevelFilter::Info);
        assert_eq!(
            logger.level_for("tunnels::ssh::session"),
            LevelFilter::Trace
        );
        // Prefix must end on a module boundary: "tunnelsx" is not "tunnels".
        assert_eq!(logger.level_for("tunnelsx"), LevelFilter::Error);
        // Unknown targets fall back to the default level.
        assert_eq!(logger.level_for("russh"), LevelFilter::Error);
    }

    #[test]
    fn bare_level_sets_the_default() {
        let logger = CaptureLogger::parse("warn,tunnels=info");
        assert_eq!(logger.level_for("anything"), LevelFilter::Warn);
        assert_eq!(logger.level_for("tunnels"), LevelFilter::Info);
    }
}
