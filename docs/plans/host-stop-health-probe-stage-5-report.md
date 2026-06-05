# Stage 5 report — HITL: empirical 502/503 + live end-to-end

Commit: _filled by parent_

## Summary
Live bring-up of the hosting + probe feature with the operator (HITL). Confirmed the SDK
hosting works in-process (all test tunnels showed `hostConnections: 1`) and the 3-state probe
renders correctly. Tuned the classifier against observed relay behaviour.

## Empirical findings
- **Hosting works.** `devtunnel list -j` showed `hostConnections: 1` for every group hosted via
  the app's Host button — the in-process `RelayTunnelHost` connect + add_port path is functional.
- **502 = upstream unreachable (the "service down" signature).** A port configured `protocol: https`
  while the local server speaks plain HTTP returns a relay **HTTP 502**, even though the local
  service is up (responds on `http://localhost`). The working port (`...-8049`, `protocol: http`)
  returned 200. This pinpointed the 3-state distinction empirically.
- **Gateway codes come from the relay, not the app.** 502/503/504 on the Public URL are emitted by
  the devtunnels relay when it cannot get a valid upstream response, so the **status code alone** is
  a reliable signal — the provisional body-string matching (`RELAY_ERROR_MARKERS`, flagged as too
  broad by the reviewer) was removed.
- **portUri is eventually consistent.** A freshly created port briefly shows no `portUri`; it appears
  on a later `show`. (Captured as follow-up in #10.)

## Changes
- `src/probe.rs` — `classify` is now `fn classify(status: Option<u16>) -> ProbeState`:
  `None → Down`, `502/503/504 → ServiceDown`, otherwise `Operational`. Removed `RELAY_ERROR_MARKERS`
  and the body download in the probe loop (status is all that's needed). Tests rewritten (9 pass).
- `CONTEXT.md` — replaced the "exact signature to confirm" note with the confirmed behaviour.

## Deviations from plan
- The plan anticipated encoding body markers from a captured 502 page. Instead, the empirical finding
  showed the gateway **status code** is the reliable signal, so body matching was dropped entirely —
  simpler and more robust than the planned approach.

## Out-of-scope items surfaced during live testing (filed separately)
- #9 — Optimistic delete (remove row immediately, revert on error).
- #10 — "Provisioning…" state on create + re-fetch the port URL after creation.
- A pre-existing tray-lifetime bug and probe-latency issue were fixed inline on this branch
  (`run_event_loop_until_quit`; immediate re-probe on target change).

## Known limitation
- A 404 is treated as `Operational` (app's own response). If a host connection silently drops, the
  relay can answer 404 itself; in practice a dropped host emits a `HostEvent` that clears the probe
  state first, so this is acceptable. Documented in `classify`.
