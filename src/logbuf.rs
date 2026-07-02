//! Process-wide bounded capture of `log` records for the Logs tab.
//!
//! [`CaptureLogger`] is installed as the global logger at startup. Every
//! enabled record is teed to stderr (preserving the previous `env_logger`
//! console behavior) and into a fixed-capacity ring buffer that the UI
//! snapshots when the port detail panel's Logs tab refreshes.

use log::{Level, LevelFilter, Log, Metadata, Record};
use std::collections::VecDeque;
use std::io::Write;
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::{Mutex, OnceLock};

/// Maximum number of captured lines kept in memory.
const CAPACITY: usize = 500;

/// One captured record: its severity (for the Logs-tab level filter) plus the
/// pre-formatted display line.
#[derive(Clone)]
struct Entry {
    level: Level,
    line: String,
}

/// A bounded FIFO of captured log entries. Kept as a plain struct (capacity is
/// a field, not the global constant) so the eviction logic is unit-testable.
struct Ring {
    entries: VecDeque<Entry>,
    capacity: usize,
}

impl Ring {
    const fn new(capacity: usize) -> Self {
        Ring {
            entries: VecDeque::new(),
            capacity,
        }
    }

    /// Appends an entry, evicting the oldest when full.
    fn push(&mut self, entry: Entry) {
        if self.entries.len() == self.capacity {
            self.entries.pop_front();
        }
        self.entries.push_back(entry);
    }

    /// Returns the lines at or above `min` severity, oldest first. A record is
    /// kept when its level is at least as severe as the filter — the same
    /// "enabled" comparison `log` uses (`Error <= Warn <= Info <= …`).
    fn snapshot(&self, min: LevelFilter) -> Vec<String> {
        self.entries
            .iter()
            .filter(|e| e.level <= min)
            .map(|e| e.line.clone())
            .collect()
    }
}

static RING: Mutex<Ring> = Mutex::new(Ring::new(CAPACITY));

/// Bounded, non-blocking sink to a dedicated stderr writer thread.
///
/// Teeing every record straight to stderr with `eprintln!` was a multi-day
/// freeze bug: `eprintln!` is a *blocking* write serialized by the global stderr
/// lock. When the app is launched from a terminal and that console pauses output
/// (a QuickEdit text selection) or its pipe backs up, the writing thread stalls
/// *holding the lock*; the next thread to log — eventually the UI thread — blocks
/// on it and the whole event loop freezes while the process stays alive.
///
/// The writer thread owns the only blocking `writeln!`; every `log()` call just
/// `try_send`s the formatted line and drops it when the channel is full. A stuck
/// console can therefore stall at most this one background thread and cost a few
/// dropped log lines — never the UI thread.
static SINK: OnceLock<SyncSender<String>> = OnceLock::new();

/// Capacity of the stderr writer channel. While the console is paused, lines
/// beyond this are dropped rather than blocking (or unboundedly growing) the
/// threads that emit them.
const SINK_CAPACITY: usize = 1024;

/// Spawns the background stderr writer thread and stores its non-blocking sender.
/// First caller wins (subsequent calls are no-ops); safe to call once from
/// [`CaptureLogger::install`].
fn init_stderr_writer() {
    let (tx, rx) = sync_channel::<String>(SINK_CAPACITY);
    if SINK.set(tx).is_err() {
        return; // already initialized
    }
    let _ = std::thread::Builder::new()
        .name("devtunnel-log-writer".to_string())
        .spawn(move || {
            let mut out = std::io::stderr();
            // A blocking write here (paused/stuck console) stalls only this
            // thread; the bounded channel drops new lines meanwhile, so no
            // logging thread ever waits on stderr.
            while let Ok(line) = rx.recv() {
                let _ = writeln!(out, "{line}");
            }
        });
}

/// Appends a record to the process-wide ring buffer.
/// Dormant in v0.1.0 (Logs-tab capture disabled); kept for re-enable + tests.
#[allow(dead_code)]
pub fn push(level: Level, line: String) {
    RING.lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(Entry { level, line });
}

/// Snapshots the process-wide ring buffer (lines at or above `min` severity,
/// oldest first). The Logs tab passes the user-chosen level from Settings.
pub fn snapshot(min: LevelFilter) -> Vec<String> {
    RING.lock().unwrap_or_else(|e| e.into_inner()).snapshot(min)
}

/// Relay/SSH chatter that is pure noise in the Logs tab — one line per client
/// connection ("Opened new client on channel N"). Hidden at capture time so it
/// never reaches the ring; matched on the formatted message text. Add further
/// noise phrases here as they surface.
/// Dormant in v0.1.0 (Logs-tab capture disabled); kept for re-enable + tests.
#[allow(dead_code)]
fn is_noise(message: &str) -> bool {
    message.contains("Opened new client on channel")
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
        // Start the decoupled stderr writer before any record can be emitted.
        init_stderr_writer();
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
        let message = record.args().to_string();
        // Technical/diagnostic content — intentionally not localized.
        let line = format!("{:<5} {} — {}", record.level(), record.target(), message);
        // Hand the line to the writer thread without ever blocking: a full
        // channel (paused/stuck console) drops the line instead of stalling this
        // — possibly the UI — thread. See `SINK` for why this matters.
        if let Some(sink) = SINK.get() {
            let _ = sink.try_send(line);
        }
        // Logs-tab capture DISABLED for stability (v0.1.0): the detail panel's
        // Logs view is turned off, so records are no longer accumulated in the
        // ring (only stderr above is kept). Restore with the panel.
        // if !is_noise(&message) {
        //     push(record.level(), line);
        // }
    }

    fn flush(&self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(level: Level, line: &str) -> Entry {
        Entry {
            level,
            line: line.to_string(),
        }
    }

    #[test]
    fn ring_evicts_oldest_when_full() {
        let mut ring = Ring::new(3);
        for i in 0..5 {
            ring.push(entry(Level::Info, &format!("line {i}")));
        }
        assert_eq!(
            ring.snapshot(LevelFilter::Trace),
            vec!["line 2", "line 3", "line 4"]
        );
    }

    #[test]
    fn ring_snapshot_preserves_insertion_order() {
        let mut ring = Ring::new(10);
        ring.push(entry(Level::Info, "a"));
        ring.push(entry(Level::Info, "b"));
        assert_eq!(ring.snapshot(LevelFilter::Info), vec!["a", "b"]);
    }

    #[test]
    fn snapshot_filters_below_min_severity() {
        let mut ring = Ring::new(10);
        ring.push(entry(Level::Error, "err"));
        ring.push(entry(Level::Info, "info"));
        ring.push(entry(Level::Debug, "dbg"));
        // Info filter keeps Error+Info, hides Debug.
        assert_eq!(ring.snapshot(LevelFilter::Info), vec!["err", "info"]);
        // Error filter keeps only Error.
        assert_eq!(ring.snapshot(LevelFilter::Error), vec!["err"]);
        // Debug filter keeps everything.
        assert_eq!(
            ring.snapshot(LevelFilter::Debug),
            vec!["err", "info", "dbg"]
        );
    }

    #[test]
    fn channel_open_chatter_is_noise() {
        assert!(is_noise("Opened new client on channel 7"));
        assert!(!is_noise("relay connected"));
    }

    #[test]
    fn global_push_and_snapshot_roundtrip() {
        push(Level::Info, "hello from test".into());
        assert!(snapshot(LevelFilter::Info)
            .iter()
            .any(|l| l == "hello from test"));
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
