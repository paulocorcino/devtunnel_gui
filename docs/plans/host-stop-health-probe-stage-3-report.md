# Stage 3 — Health probe engine (ureq, 3-state) — Post-stage report

**Backlog items:** S3-loop, S3-classify, S3-events
**Commit:** _filled by parent in the End-to-end summary table_
**Plan:** host-stop-health-probe.md

## Files changed

- `src/probe.rs` (new) — Full probe engine behind `#![cfg(feature = "hosting")]`.
  Defines `ProbeState`, `ProbeEvent`, `ProbeCommand`, `ProbeTarget`, the pure
  `classify()` function, the `spawn()` thread function, and 5 `#[cfg(test)]` unit tests.
  Named constant `RELAY_ERROR_MARKERS` holds the provisional 502/503 body markers.
- `src/main.rs` — Added `#[cfg(feature = "hosting")] mod probe;` alongside the existing
  module declarations (line 8).

## Gate results

| Gate | Result |
|------|--------|
| `cargo build` (default, no feature) | PASS |
| `cargo build --features hosting` (PATH-prepended) | PASS (7 dead_code warnings — expected; Stage 4 wires them) |
| `cargo test --features hosting` | PASS — 10 tests: 5 `probe::tests::classify_*` + 5 pre-existing `devtunnel::tests::*` |
| `cargo clippy --features hosting` | PASS — no new errors; pre-existing warnings in `locale.rs`/`main.rs` are out of scope |
| `cargo fmt -- src/probe.rs` + revert out-of-scope fmt drift | PASS — only `probe.rs` reformatted; `src/bin/host_spike.rs` and `src/locale.rs` reverted |

### classify test names (all passing)
- `probe::tests::classify_operational_on_2xx`
- `probe::tests::classify_service_down_on_relay_502`
- `probe::tests::classify_service_down_on_relay_503`
- `probe::tests::classify_down_on_network_error`
- `probe::tests::classify_down_on_502_without_relay_marker`

## Acceptance criteria audit

- [x] `ProbeState { Operational, ServiceDown, Down }` defined
- [x] `ProbeEvent::Status { tunnel_id, port, state }` defined
- [x] `ProbeCommand::SetTargets` and `SetInterval` defined
- [x] Pure `fn classify(status: Option<u16>, body: &str) -> ProbeState` with no side effects
- [x] Named constant `RELAY_ERROR_MARKERS` for provisional 502/503 signature
- [x] Background thread with mpsc command channel and 60 s default interval
- [x] 5 s connect + read timeout on the `ureq::Agent`
- [x] `#[cfg(test)]` unit tests covering all 3 branches (Operational / ServiceDown / Down)
- [x] All probe code behind `#[cfg(feature = "hosting")]`
- [x] `#[cfg(feature = "hosting")] mod probe;` added to `src/main.rs`
- [x] Default `cargo build` compiles without the feature

## Deviations from plan

- Added an extra test `classify_down_on_502_without_relay_marker` (5 tests total vs "3 branches"
  in the plan). The plan says "covering all 3 branches"; the extra test verifies the conservative
  fallback within the `ServiceDown` branch. Not a scope deviation — it strengthens the seam.
- `cargo fmt` also touched `src/bin/host_spike.rs` and `src/locale.rs` (pre-existing fmt drift).
  Those files were reverted with `git checkout --` before staging, as required by working-tree policy.

## Surprises / notes

- The `ureq` 2.x API uses `Error::Status(code, resp)` for non-2xx responses rather than returning
  an `Ok`. The probe loop handles this correctly to capture body text for 502/503 classification.
- `RELAY_ERROR_MARKERS` markers (`"devtunnels"`, `"tunnel"`, `"Bad Gateway"`, `"Service Unavailable"`)
  are provisional. Stage 5 (HITL) must confirm against a live relay error page; the `classify()`
  seam is intentionally narrow so only the constant changes.
- The sleep loop checks commands every second to stay responsive to `SetTargets`/`SetInterval`;
  this avoids a 60 s lag when the hosted group list changes.
