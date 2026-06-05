# Stage 2 — SDK host engine (connect/keep-alive/stop) — Post-stage report

**Backlog items:** S2-engine, S2-connect, S2-keepalive, S2-remint, S2-stop
**Commit:** _filled by parent in the End-to-end summary table_
**Plan:** host-stop-health-probe.md

## Files changed
- `src/host/engine.rs` (new, `#[cfg(feature = "hosting")]`) — SDK-backed host engine.
  - `start(events) -> Sender<HostCommand>`: spawns the `devtunnel-host` OS thread owning a
    current-thread tokio runtime driven inside a `LocalSet` (the host task holds a non-`Send`
    `RelayTunnelHost`/russh session across awaits, so `spawn_local` is required).
  - `run(...)`: command loop. A small bridge thread forwards the blocking std `Receiver` onto a
    tokio mpsc so the loop can `await`. Tracks active per-group tasks in a `HashMap<tunnel_id,
    JoinHandle>`. `Host` ignores a duplicate while a task is live, collects ports, spawns
    `host_group`. `Stop` aborts the task and emits `HostState::Stopped`.
  - `collect_ports`: reuses `devtunnel::fetch_rows` filtered by `tunnel_id` (port > 0) → `Vec<u16>`.
  - `host_group`: connect → add ports → keep alive via `tokio::select!` on the `RelayHandle` future
    vs a 20h re-mint timer; reconnects with exponential backoff (2s→60s) on drop/timer/error,
    emitting `Connecting`/`Reconnecting`/`Hosting`/`Error` transitions.
  - `connect_once`: mints `host` + `manage:ports` tokens, builds `TunnelManagementClient`
    (`Authorization::Tunnel`), `TunnelLocator::ID` via `split_locator`, `RelayTunnelHost::connect`,
    then `add_port` per port — the proven `host_spike.rs` sequence.
- `src/host/mod.rs` — added `#[cfg(feature = "hosting")] mod engine;`; replaced the Stage-1
  placeholder hosting `spawn` with one that calls `engine::start(events)` and returns an
  `EngineHost` whose `send` forwards to the engine command channel. `NoopHost` (non-hosting)
  variant unchanged.

## Gate results
- `cargo build` (default, engine excluded): **pass** (Finished in 7.48s).
- `cargo build --features hosting` (PATH-prepended): **pass** (Finished in 15.77s).
- `cargo clippy --features hosting`: **pass for `src/host/`** — 0 warnings in host files. The 5
  reported warnings are all pre-existing, located in `src/main.rs` (402/412/413/422) and
  `src/locale.rs:76`; out of this stage's scope, left untouched.
- `cargo test` (default): **pass** — 5 passed (existing `sanitize_tunnel_id` tests).
- `cargo fmt -- src/host/engine.rs src/host/mod.rs`: applied; host files clean.

## Acceptance criteria audit
- S2-engine: dedicated OS thread + current-thread tokio runtime + mpsc command receiver. ✓
- S2-connect: mint host + manage:ports tokens → mgmt client → locator → connect → add_port per port. ✓
- S2-keepalive: `select!` on `RelayHandle` future with reconnect + exponential backoff on drop. ✓
- S2-remint: 20h timer re-mints tokens and reconnects before the ~24h expiry. ✓
- S2-stop: `Stop` aborts the group task (drops relay handle) → emits `HostState::Stopped`. ✓
- All transitions emitted via `HostEvent::State`. ✓
- All new SDK code behind `#[cfg(feature = "hosting")]`; default build still compiles. ✓

## Deviations from plan
- None functionally. The plan suggested "fetch ports via `devtunnel::fetch_rows` filtered by
  tunnel_id, or a `show`-based helper" — chose the `fetch_rows` path (reuses existing code).
- Used `LocalSet` + `spawn_local` (not in the plan text) because `RelayTunnelHost`/russh state is
  not `Send` and cannot cross to a worker thread; this keeps the per-group task on the runtime
  thread, matching the spike's single-thread model while still supporting multiple groups.

## Surprises / notes
- Open question (tunnel-id/cluster shape): resolved pragmatically per the plan default —
  `split_locator` splits at the last `.` giving `(cluster, id)`. Not yet validated against live CLI
  (live hosting is Stage 5/HITL); if `tunnelId` does not carry the cluster suffix, `connect_once`
  surfaces a clear `Error` and Stage 5 can switch to deriving cluster from the port URI host.
- `Locale` is not `Send`, so it is constructed inside the engine thread (and rebuilt in
  `connect_once`) from `system_locale()` rather than passed across the thread boundary.
- Token minting / `fetch_rows` are blocking subprocess calls invoked from async context on a
  current-thread runtime; during a connect they briefly block other groups' progress. Acceptable
  for this stage (mirrors the spike); a future refactor could move them to `spawn_blocking`.
