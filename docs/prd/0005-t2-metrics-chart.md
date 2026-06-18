---
id: "0005"
track: "t2"
status: "active"
related_adrs: []
milestone: "PRD-0005: Metrics line chart"
---

# PRD-0005 — Metrics line chart

> Part of track `t2` · See [roadmap](../roadmap.md)

## Problem / Why now

The per-port Metrics tab shows only the latest numeric readout (upload/download totals
+ rates, active connections). The persistence groundwork just landed:
`src/metrics_store.rs` appends a timestamped `Sample` per successful poll to
`state_dir()/metrics/<tunnel>_<port>.json`, capped at ~1h of history, with real numbers
parsed from the tunnel-level traffic strings by `fetch_port_status`. The history exists
and is unused — time to visualize it.

## Goals

- Render a simple line chart (download/upload over time) on the Metrics tab, scaled to the panel, alongside the existing numeric readout.

## Non-goals

- Per-port traffic breakdown (traffic is reported per **tunnel**, not per port).
- Long-term history beyond the existing ~1h cap.
- Configurable time windows or chart types.

## User / use case

A user hosting a Group who wants to see traffic trend over the last hour, not just an
instantaneous number.

## Requirements / expected behavior

- Read the persisted `Sample` history for the selected port and draw two lines (download, upload) with a Slint `Path`, scaled to the panel.
- Keep the numeric readout; the chart augments it.
- For multi-port groups the chart shows the group's aggregate traffic on each port's panel — label it as group-level to avoid implying per-port isolation.

## Definition of Done

- [ ] Metrics tab shows a download/upload line chart from persisted samples.
- [ ] No regression to the existing numeric readout.
- [ ] Multi-port aggregate behavior is labeled, not silently misleading.
- [ ] All issues in milestone `PRD-0005` are closed.

## Issue breakdown

| Issue | Title | Type | Status |
|-------|-------|------|--------|
| #41 | Metrics line chart from persisted samples | AFK | open (ready-for-agent) |

## Out of scope / explicitly rejected

- True per-port traffic isolation — the service does not expose it; traffic lives at the tunnel level.

## Links

- Roadmap: [docs/roadmap.md](../roadmap.md)
