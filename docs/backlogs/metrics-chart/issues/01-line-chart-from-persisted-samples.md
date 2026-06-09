# 01 — Metrics line chart from persisted samples (0.2.0)

Status: ready-for-agent

## Context

For 0.1.0 the per-port Metrics tab shows only the latest numeric readout
(upload/download totals + rates, active connections).

**Root cause of the empty readout (resolved):** the service does **not** return a
numeric per-port `status` object. Traffic lives at the **tunnel** level as
human-readable strings (`uploadTotal: "4402 KB"`,
`currentUploadRate: "0 MB/s (limit: 20 MB/s)"`), and the port `status` is a
summary string like `"4 client connections"`.
[`fetch_port_status`](../../../src/devtunnel.rs) now parses those tunnel-level
strings into bytes / bytes-per-second and reads the connection count from the
port status string, so the readout populates and feeds the sample store.

Groundwork already landed:

- **Persistence** — [`src/metrics_store.rs`](../../../src/metrics_store.rs)
  appends a timestamped `Sample` per successful poll to
  `state_dir()/metrics/<tunnel>_<port>.json`, capped at ~1h of history. These are
  now real numbers, ready for the chart.

## Goal

Render a simple line chart on the Metrics tab from the persisted samples:
download/upload over time, alongside the existing connection/total readouts.

## Scope / approach

- Read the persisted `Sample` history for the selected port and draw two lines
  (download, upload) with a Slint `Path`, scaled to the panel.
- Keep the numeric readout; the chart augments it.
- Data is confirmed flowing (tunnel-level traffic strings, parsed to numbers), so
  this is now unblocked.

## Notes

- Traffic is reported per **tunnel**, not per port. For multi-port groups the
  chart would show the group's aggregate traffic on each port's panel. Consider
  labeling it as group-level, or aggregating in the UI.
