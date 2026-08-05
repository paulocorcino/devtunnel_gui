//! Panic capture and the user-driven GitHub report flow.
//!
//! A process-wide panic hook (installed first thing in `main`) serializes every
//! panic — message, location, thread, backtrace and the recent Warn/Error log
//! records — to a `crash-<unix>-<pid>.json` file under [`state::state_dir`].
//! Nothing is transmitted: the file only sits on disk.
//!
//! On the next start, [`take_pending`] picks up the newest report, clears the
//! pending files and keeps a copy as `last-crash.json` so the user can attach
//! the full report to an issue. The UI raises a banner offering "Report on
//! GitHub", which opens [`issue_url`] — a pre-filled *new issue* form — in the
//! browser. The user reviews and submits it, so no telemetry leaves the machine
//! without an explicit, visible action (this is what keeps the flow inside
//! Store privacy policy without a consent prompt).
//!
//! Why a panic hook and not the Windows crash report: a Rust panic is not a
//! crash to Windows (the process exits with code 101), so it never reaches Watson
//! and shows up nowhere. Genuine access violations *are* reported to Windows but
//! land in Partner Center as "Uncategorized" without published symbols, and never
//! carry app context. This hook covers the panic half with full context; native
//! faults still need a minidump handler.

use crate::logbuf;
use crate::state;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// The repo's "new issue" form. Query parameters pre-fill title, body and labels.
const ISSUE_NEW_URL: &str = "https://github.com/paulocorcino/devtunnel_gui/issues/new";

/// Label applied to filed crash reports. Must exist in the repo or GitHub
/// rejects the pre-filled form (see `docs/agents/triage-labels.md`).
const ISSUE_LABEL: &str = "needs-triage";

/// File-name prefix of a pending (not yet surfaced) crash report.
const CRASH_PREFIX: &str = "crash-";

/// Where the most recent report is kept after being surfaced, so the user can
/// attach the untruncated JSON to the issue.
const LAST_CRASH_FILE: &str = "last-crash.json";

/// Maximum characters of issue body put in the URL. Browsers and GitHub cap the
/// request line (~8 KB); percent-encoding a backtrace inflates it roughly 3x, so
/// the raw body is trimmed well below that and the rest stays in the JSON file.
const MAX_BODY_CHARS: usize = 3000;

/// One captured panic, as persisted between runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrashReport {
    /// Build that panicked (`GIT_VERSION`).
    pub version: String,
    /// Wall-clock capture time, Unix seconds.
    pub when_unix: u64,
    /// Name of the panicking thread (`<unnamed>` for anonymous ones).
    pub thread: String,
    /// The panic payload, when it is a string.
    pub message: String,
    /// `file:line:column` of the panic site, empty when unavailable.
    pub location: String,
    /// Captured backtrace. Symbolized only when the build carries debug info —
    /// `[profile.release] debug = 1` keeps line tables for exactly this.
    pub backtrace: String,
    /// Recent Warn/Error log records, oldest first (see [`logbuf::crash_context`]).
    pub logs: Vec<String>,
}

/// Installs the process-wide panic hook. Call once, before any other thread
/// starts. The previously installed hook still runs afterwards, so the default
/// stderr message is preserved.
pub fn install_hook() {
    install_hook_writing_to(state::state_dir());
}

fn install_hook_writing_to(dir: PathBuf) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // Everything here is best-effort and non-panicking: a panic inside the
        // hook aborts the process and would lose the report we are writing.
        let report = capture(info);
        if let Err(e) = write_report(&dir, &report) {
            // stderr directly, not `log::` — the logger may be the thing that
            // panicked, and this is the last-resort diagnostic.
            eprintln!("crash: failed to persist report: {e}");
        }
        previous(info);
    }));
}

/// Builds a [`CrashReport`] from the hook's panic info plus ambient context.
fn capture(info: &std::panic::PanicHookInfo<'_>) -> CrashReport {
    let payload = info.payload();
    let message = payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "<non-string panic payload>".to_string());

    CrashReport {
        version: env!("GIT_VERSION").to_string(),
        when_unix: now_unix_secs(),
        thread: std::thread::current()
            .name()
            .unwrap_or("<unnamed>")
            .to_string(),
        message,
        location: info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_default(),
        // `force_capture` ignores RUST_BACKTRACE: a shipped build has no such
        // variable set, and a backtrace-less report is barely actionable.
        backtrace: trim_backtrace(&std::backtrace::Backtrace::force_capture().to_string()),
        logs: logbuf::crash_context(),
    }
}

/// Drops the leading frames that belong to the capture and panic machinery, so
/// the report opens on the frame that actually panicked.
///
/// A backtrace taken inside the hook starts with `Backtrace::force_capture`,
/// this module and `std::panicking::*` — a dozen frames of noise before the
/// first line of ours. That noise is a *contiguous prefix*, so the scan stops at
/// the first frame that is not machinery. Matching the last machinery frame
/// instead would cut far too deep: `std::rt` unwinds through
/// `std::panicking::try` at the very bottom of every stack, below `main`.
///
/// When the shape is unexpected (no prefix, or nothing but machinery) the
/// backtrace is kept whole rather than risking the loss of the only evidence.
fn trim_backtrace(backtrace: &str) -> String {
    let lines: Vec<&str> = backtrace.lines().collect();
    let frame_starts: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| is_frame_start(l))
        .map(|(i, _)| i)
        .collect();

    let first_real = frame_starts
        .iter()
        .position(|&i| !is_machinery_frame(lines[i]));
    match first_real {
        Some(0) | None => backtrace.to_string(),
        Some(n) => lines[frame_starts[n]..].join("\n"),
    }
}

/// Whether a frame belongs to the capture/panic machinery rather than the app.
fn is_machinery_frame(name: &str) -> bool {
    const MACHINERY: [&str; 6] = [
        "backtrace_rs",
        "std::backtrace",
        "devtunnel_gui::crash",
        "std::panicking",
        "core::panicking",
        "rust_begin_unwind",
    ];
    MACHINERY.iter().any(|m| name.contains(m))
}

/// Whether `line` opens a backtrace frame (`   7: some::function`).
fn is_frame_start(line: &str) -> bool {
    let t = line.trim_start();
    let digits = t.len() - t.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    digits > 0 && t[digits..].starts_with(": ")
}

/// Writes `report` to a uniquely named pending file in `dir`. The pid suffix
/// keeps two instances crashing in the same second from overwriting each other.
fn write_report(dir: &Path, report: &CrashReport) -> anyhow::Result<()> {
    fs::create_dir_all(dir)?;
    let name = format!(
        "{CRASH_PREFIX}{:013}-{}.json",
        report.when_unix,
        std::process::id()
    );
    // Written directly (no temp + rename): the process is already dying, and a
    // truncated report simply fails to parse and is discarded on the next start.
    fs::write(dir.join(name), serde_json::to_string_pretty(report)?)?;
    Ok(())
}

/// Consumes the newest pending crash report, if any.
///
/// Clears every pending file (so the banner is raised once per crash) and keeps
/// the newest as `last-crash.json` for the user to attach.
pub fn take_pending() -> Option<CrashReport> {
    take_pending_in(&state::state_dir())
}

fn take_pending_in(dir: &Path) -> Option<CrashReport> {
    let mut pending: Vec<PathBuf> = fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| is_pending_report(p))
        .collect();
    // Names are `crash-<zero-padded unix>-<pid>.json`, so lexical order is
    // chronological and the last entry is the most recent crash.
    pending.sort();
    let newest = pending.pop()?;

    let report = fs::read_to_string(&newest)
        .ok()
        .and_then(|text| serde_json::from_str::<CrashReport>(&text).ok());

    for stale in &pending {
        let _ = fs::remove_file(stale);
    }
    // Promote the newest to the stable name. `rename` fails on Windows when the
    // destination exists, so clear it first; if the move still fails, drop the
    // file rather than leaving it pending forever.
    let keep = dir.join(LAST_CRASH_FILE);
    let _ = fs::remove_file(&keep);
    if fs::rename(&newest, &keep).is_err() {
        let _ = fs::remove_file(&newest);
    }
    report
}

/// Whether `path` is a pending crash report written by [`write_report`].
fn is_pending_report(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with(CRASH_PREFIX) && n.ends_with(".json"))
}

/// Builds the pre-filled GitHub "new issue" URL for `report`.
pub fn issue_url(report: &CrashReport) -> String {
    format!(
        "{ISSUE_NEW_URL}?labels={}&title={}&body={}",
        percent_encode(ISSUE_LABEL),
        percent_encode(&issue_title(report)),
        percent_encode(&issue_body(report)),
    )
}

/// One-line issue title: the first line of the panic message, bounded so the
/// GitHub title field stays readable.
fn issue_title(report: &CrashReport) -> String {
    let first = report.message.lines().next().unwrap_or("").trim();
    let summary = if first.is_empty() {
        "unexpected exit"
    } else {
        first
    };
    format!("Crash: {} ({})", truncate(summary, 90), report.version)
}

/// The pre-filled issue body: a prompt for the user's own description followed
/// by the collected technical context, trimmed to [`MAX_BODY_CHARS`].
fn issue_body(report: &CrashReport) -> String {
    let logs = if report.logs.is_empty() {
        "(none captured)".to_string()
    } else {
        report.logs.join("\n")
    };
    let body = format!(
        "### What were you doing when it crashed?\n\
         \n\
         _Please describe it here — it is usually the missing half of the report._\n\
         \n\
         ---\n\
         \n\
         <!-- Collected automatically by TunnelDeck. Review it and remove anything you would rather not share. -->\n\
         \n\
         | | |\n\
         |---|---|\n\
         | Version | `{version}` |\n\
         | When | {when} |\n\
         | Thread | `{thread}` |\n\
         | Location | `{location}` |\n\
         \n\
         **Panic message**\n\
         ```\n{message}\n```\n\
         \n\
         **Backtrace**\n\
         ```\n{backtrace}\n```\n\
         \n\
         **Recent log entries**\n\
         ```\n{logs}\n```\n\
         \n\
         The full, untruncated report is on your machine at `%APPDATA%\\devtunnel-gui\\{LAST_CRASH_FILE}` — attaching it helps.\n",
        version = report.version,
        when = format_utc(report.when_unix),
        thread = report.thread,
        location = if report.location.is_empty() { "unknown" } else { &report.location },
        message = report.message,
        backtrace = report.backtrace,
    );
    truncate_body(&body)
}

/// Trims the body to [`MAX_BODY_CHARS`], pointing at the on-disk report for the
/// rest. Only the tail of the backtrace is ever lost — the header, message and
/// the first (most relevant) frames always survive.
fn truncate_body(body: &str) -> String {
    if body.chars().count() <= MAX_BODY_CHARS {
        return body.to_string();
    }
    let cut: String = body.chars().take(MAX_BODY_CHARS).collect();
    format!("{cut}\n```\n\n_(truncated — the full report is in `{LAST_CRASH_FILE}`, please attach it)_\n")
}

/// Truncates `s` to `max` characters, appending an ellipsis when it was cut.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect::<String>() + "…"
}

/// Percent-encodes `s` for use as a URL query-parameter value (RFC 3986
/// unreserved set kept verbatim, everything else escaped).
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Current wall-clock time in Unix seconds (0 if the clock predates the epoch).
fn now_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Formats Unix seconds as `YYYY-MM-DD HH:MM:SS UTC`. Done by hand rather than
/// with a date crate: this is the only timestamp the app ever formats.
fn format_utc(secs: u64) -> String {
    let (y, m, d) = civil_from_days((secs / 86_400) as i64);
    let rem = secs % 86_400;
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02} UTC",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Days-since-epoch to `(year, month, day)` — Howard Hinnant's `civil_from_days`,
/// exact for the whole proleptic Gregorian calendar.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    // Shift the epoch to 0000-03-01 so leap days land at the end of the cycle.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report() -> CrashReport {
        CrashReport {
            version: "v0.1.0".into(),
            when_unix: 1_770_000_000,
            thread: "main".into(),
            message: "index out of bounds: the len is 0 but the index is 3".into(),
            location: "src/main.rs:42:9".into(),
            backtrace: "   0: devtunnel_gui::main\n   1: core::ops::function::FnOnce".into(),
            logs: vec!["WARN  devtunnel_gui — relay dropped".into()],
        }
    }

    /// A unique temp directory per test (no collision across parallel tests).
    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "devtunnel-gui-crash-{}-{name}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn take_pending_returns_none_without_reports() {
        let dir = temp_dir("empty");
        assert_eq!(take_pending_in(&dir), None);
    }

    #[test]
    fn write_then_take_round_trips_and_clears_pending() {
        let dir = temp_dir("roundtrip");
        write_report(&dir, &report()).unwrap();

        assert_eq!(take_pending_in(&dir), Some(report()));
        // The report is surfaced exactly once…
        assert_eq!(take_pending_in(&dir), None);
        // …but stays on disk under the stable name for the user to attach.
        assert!(dir.join(LAST_CRASH_FILE).is_file());
    }

    #[test]
    fn take_pending_picks_the_newest_and_drops_the_rest() {
        let dir = temp_dir("newest");
        let mut older = report();
        older.when_unix = 1_760_000_000;
        older.message = "older crash".into();
        write_report(&dir, &older).unwrap();
        write_report(&dir, &report()).unwrap();

        let taken = take_pending_in(&dir).unwrap();
        assert_eq!(taken.when_unix, 1_770_000_000);
        // Both pending files are gone — an old crash never re-raises the banner.
        assert_eq!(take_pending_in(&dir), None);
    }

    #[test]
    fn unparseable_report_is_discarded() {
        let dir = temp_dir("garbage");
        fs::write(dir.join("crash-0000000000001-1.json"), "not json").unwrap();
        assert_eq!(take_pending_in(&dir), None);
        // …and cleared, so it cannot block a later real report.
        assert_eq!(fs::read_dir(&dir).unwrap().flatten().count(), 1); // last-crash.json only
    }

    #[test]
    fn issue_url_is_encoded_and_carries_the_context() {
        let url = issue_url(&report());
        assert!(url.starts_with(ISSUE_NEW_URL));
        assert!(url.contains("labels=needs-triage"));
        // No raw spaces or newlines survive into the URL.
        assert!(!url.contains(' ') && !url.contains('\n'));
        // The panic message and location are carried through, encoded.
        assert!(url.contains(&percent_encode("index out of bounds")));
        assert!(url.contains(&percent_encode("src/main.rs:42:9")));
    }

    #[test]
    fn issue_title_summarizes_the_panic() {
        assert_eq!(
            issue_title(&report()),
            "Crash: index out of bounds: the len is 0 but the index is 3 (v0.1.0)"
        );
        let mut long = report();
        long.message = "x".repeat(200);
        assert!(issue_title(&long).contains('…'));
    }

    #[test]
    fn body_is_bounded_for_the_url() {
        let mut huge = report();
        huge.backtrace = "frame\n".repeat(5_000);
        let body = issue_body(&huge);
        assert!(body.chars().count() <= MAX_BODY_CHARS + 200);
        assert!(body.contains(LAST_CRASH_FILE));
        // Percent-encoding inflates the body ~3x; the whole URL must still fit
        // the ~8 KB request line browsers and GitHub accept.
        assert!(issue_url(&huge).len() < 8_000);
        // The head of the report (message, first frames) is what survives.
        assert!(body.contains("index out of bounds"));
    }

    #[test]
    fn backtrace_starts_at_the_frame_that_panicked() {
        let raw = "\
   0: std::backtrace::Backtrace::force_capture
             at /rustc/library/std/src/backtrace.rs:312
   1: devtunnel_gui::crash::capture
   2: std::panicking::rust_panic_with_hook
   3: core::panicking::panic_fmt
   4: devtunnel_gui::host::connect
             at ./src/host/mod.rs:88
   5: std::panicking::try
   6: std::rt::lang_start_internal";
        let trimmed = trim_backtrace(raw);
        assert!(trimmed.starts_with("   4: devtunnel_gui::host::connect"));
        // The `at` line of the surviving frame comes along.
        assert!(trimmed.contains("./src/host/mod.rs:88"));
        assert!(!trimmed.contains("force_capture"));
        // Regression: `std::rt` unwinds through `std::panicking::try` *below*
        // main, so matching the last machinery frame would drop the app's own
        // frames and keep only the runtime's.
        assert!(trimmed.contains("lang_start_internal"));
    }

    #[test]
    fn backtrace_without_panic_frames_is_kept_whole() {
        // Unexpected shape (or an empty capture): never throw evidence away.
        let raw = "   0: devtunnel_gui::main\n   1: main";
        assert_eq!(trim_backtrace(raw), raw);
        assert_eq!(trim_backtrace(""), "");
        // …and neither when the machinery is all there is.
        let only = "   0: std::panicking::rust_panic_with_hook";
        assert_eq!(trim_backtrace(only), only);
    }

    #[test]
    fn percent_encoding_keeps_unreserved_and_escapes_the_rest() {
        assert_eq!(percent_encode("aZ0-_.~"), "aZ0-_.~");
        assert_eq!(percent_encode("a b/c\n"), "a%20b%2Fc%0A");
        // Multi-byte characters are escaped per UTF-8 byte.
        assert_eq!(percent_encode("é"), "%C3%A9");
    }

    #[test]
    fn formats_utc_timestamps() {
        assert_eq!(format_utc(0), "1970-01-01 00:00:00 UTC");
        assert_eq!(format_utc(1_770_000_000), "2026-02-02 02:40:00 UTC");
        // Leap day.
        assert_eq!(format_utc(1_709_164_800), "2024-02-29 00:00:00 UTC");
    }
}
