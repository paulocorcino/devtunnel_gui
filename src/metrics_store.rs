//! Bounded per-port metrics time-series, persisted as JSON.
//!
//! Groundwork for the 0.2.0 download/upload line chart: each successful metrics
//! poll appends a [`Sample`] for the port, and a bounded history is kept on disk
//! under `state_dir()/metrics/<tunnel>_<port>.json`. The chart will read this
//! history; until then nothing consumes it but the capture, so a write failure
//! is best-effort (logged, never fatal).
//!
//! NOTE: this rewrites the whole file each append, which is fine at the current
//! ~3s cadence and small cap. If the cadence or cap grows, switch to an in-memory
//! buffer flushed periodically.
//!
//! Currently dormant: per-port metrics polling is disabled in v0.1.0 for
//! stability, so `append` has no live caller (only tests). Kept ready for the
//! 0.2.0 chart — hence the module-level dead-code allowance.
#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::devtunnel::PortMetrics;
use crate::state;

/// Maximum samples kept per port (~1 hour at a 5 s cadence).
const MAX_SAMPLES: usize = 720;

/// One point in a port's metrics history.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Sample {
    /// Unix epoch seconds when the sample was taken.
    pub ts_secs: u64,
    pub upload_total: Option<f64>,
    pub download_total: Option<f64>,
    pub upload_rate: Option<f64>,
    pub download_rate: Option<f64>,
    pub connections: Option<f64>,
}

/// `state_dir()/metrics`.
fn metrics_dir() -> PathBuf {
    state::state_dir().join("metrics")
}

/// Per-port history file. The tunnel id (e.g. `name.cluster`) is sanitized to a
/// filename-safe form so it can never escape the metrics directory.
fn file_for(tunnel_id: &str, port: i32) -> PathBuf {
    let safe: String = tunnel_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    metrics_dir().join(format!("{safe}_{port}.json"))
}

/// Loads a port's persisted history. Missing or invalid files yield an empty
/// history rather than failing.
pub fn load(tunnel_id: &str, port: i32) -> Vec<Sample> {
    match std::fs::read_to_string(file_for(tunnel_id, port)) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

/// Appends a sample for `(tunnel_id, port)`, trimming to [`MAX_SAMPLES`].
/// Best-effort: any I/O error is logged and swallowed.
pub fn append(tunnel_id: &str, port: i32, metrics: &PortMetrics, ts_secs: u64) {
    let mut samples = load(tunnel_id, port);
    samples.push(Sample {
        ts_secs,
        upload_total: metrics.upload_total,
        download_total: metrics.download_total,
        upload_rate: metrics.upload_rate,
        download_rate: metrics.download_rate,
        connections: metrics.connection_count,
    });
    let len = samples.len();
    if len > MAX_SAMPLES {
        samples.drain(0..len - MAX_SAMPLES);
    }

    let dir = metrics_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        log::warn!("metrics_store: create dir {}: {e}", dir.display());
        return;
    }
    let path = file_for(tunnel_id, port);
    match serde_json::to_string(&samples) {
        Ok(json) => {
            if let Err(e) = std::fs::write(&path, json) {
                log::warn!("metrics_store: write {}: {e}", path.display());
            }
        }
        Err(e) => log::warn!("metrics_store: serialize: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metrics(up: f64, down: f64) -> PortMetrics {
        PortMetrics {
            upload_rate: Some(up),
            download_rate: Some(down),
            upload_total: Some(up * 10.0),
            download_total: Some(down * 10.0),
            connection_count: Some(1.0),
        }
    }

    #[test]
    fn sample_roundtrips_through_json() {
        let s = Sample {
            ts_secs: 42,
            upload_total: Some(100.0),
            download_total: None,
            upload_rate: Some(5.0),
            download_rate: Some(7.0),
            connections: Some(2.0),
        };
        let json = serde_json::to_string(&s).unwrap();
        let back: Sample = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn file_for_sanitizes_unsafe_chars() {
        let p = file_for("my/tunnel id", 8080);
        let name = p.file_name().unwrap().to_string_lossy();
        assert_eq!(name, "my_tunnel_id_8080.json");
    }

    #[test]
    fn trims_to_max_samples_when_pushing() {
        // Simulate the trim logic directly (avoids touching the real state dir).
        let mut samples: Vec<Sample> = (0..MAX_SAMPLES as u64)
            .map(|i| Sample {
                ts_secs: i,
                upload_total: None,
                download_total: None,
                upload_rate: None,
                download_rate: None,
                connections: None,
            })
            .collect();
        let m = metrics(1.0, 2.0);
        samples.push(Sample {
            ts_secs: 9999,
            upload_total: m.upload_total,
            download_total: m.download_total,
            upload_rate: m.upload_rate,
            download_rate: m.download_rate,
            connections: m.connection_count,
        });
        let len = samples.len();
        if len > MAX_SAMPLES {
            samples.drain(0..len - MAX_SAMPLES);
        }
        assert_eq!(samples.len(), MAX_SAMPLES);
        // Oldest dropped, newest kept.
        assert_eq!(samples.last().unwrap().ts_secs, 9999);
        assert_eq!(samples.first().unwrap().ts_secs, 1);
    }
}
