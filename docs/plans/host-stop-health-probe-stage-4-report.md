# Stage 4 — UI + main.rs wiring (toggle + badges) — Post-stage report

**Backlog items:** S4-strings, S4-toggle, S4-badge, S4-pump
**Commit:** _filled by parent in the End-to-end summary table_
**Plan:** host-stop-health-probe.md

## Files changed
- `i18n/en-US/app.ftl` — added `btn-host`, `btn-stop`, `status-hosting`, `status-stopped`,
  `badge-operational`, `badge-service-down`, `badge-down` (English values).
- `ui/strings.slint` — added one `in property <string>` per new key with English defaults
  (kept in lockstep with the ftl keys).
- `src/main.rs`:
  - `apply_strings` — added `s.set_*` for all 7 new strings.
  - New `LiveState` struct (cached rows + per-port probe status map + per-group host state map),
    shared on the UI thread via `Rc<RefCell<…>>`.
  - `derive_status` / `derive_host_state` — derive a row's `status` and toggle `host-state`
    from the latest probe/host events (probe wins; falls back to host state then `"idle"`).
  - `map_host_state` (all builds), `map_probe_state` + `hosting_targets` (`#[cfg(hosting)]`).
  - Spawn `host::spawn` (every build; no-op without the feature) and, under
    `#[cfg(feature = "hosting")]`, `probe::spawn`; `app.set_hosting_enabled(true)` only in the
    hosting build (default build leaves the toggle disabled).
  - `on_host` / `on_stop` callbacks — forward `HostCommand`, optimistically update host state,
    rebuild rows; Stop also clears the group's probe results.
  - Timer (150 ms) pump — drains `HostEvent` and (cfg-gated) `ProbeEvent` receivers, updates the
    state maps, re-points the probe at currently-hosting groups' URLs on load/host change, and
    rebuilds rows when derived state changes.
  - Refactored `apply_rows` to cache the load into `LiveState` and delegate row building to a new
    `rebuild_rows` helper (replaces the hard-coded `status: "idle"`).
- `ui/app-window.slint`:
  - `PortRow` gained a `host-state` field.
  - `AppWindow` gained `hosting-enabled` (in property, default false) and `host`/`stop` callbacks.
  - Status column now shows `StatusDot` + a localized `Pill` badge (Operational/Service down/Down)
    when the row has a probe result.
  - Per-group Host/Stop toggle (`TxtButton`) whose label flips on `host-state`, `enabled` bound to
    `hosting-enabled`. All styling via `Theme.*` / existing components; no hard-coded color/size.

## Gate results
- `cargo build` (default) — pass.
- `cargo build --features hosting` (PATH-prepended) — pass (one pre-existing `probe.rs`
  `SetInterval` dead-code warning, out of scope).
- `cargo test` — pass (5 tests; missing-i18n-key panic would surface here — none).
- `cargo test --features hosting` — pass (10 tests incl. `classify`).
- `cargo clippy` and `cargo clippy --features hosting` — no NEW warnings; the 4 reported
  (`needless_borrows_for_generic_args` in `build_tray_menu`) pre-exist on the clean tree
  (verified via `git stash` + clippy).

## Acceptance criteria audit
- S4-strings — 7 keys added across ftl → strings.slint → apply_strings, in lockstep. ✓
- S4-toggle — per-group Host/Stop toggle, label driven by host state, disabled in default build. ✓
- S4-badge — per-port `StatusDot` + localized badge; ProbeState mapped Operational→ok,
  ServiceDown→warn, Down→down; hosting-but-unprobed→host; not hosted→idle. ✓
- S4-pump — both event receivers drained in the existing 150 ms Timer; `apply_rows` derives
  `status` from live state instead of the hard-coded `"idle"`. ✓
- No raw UI string literal in Rust/Slint — verified via grep; only status/state identifiers
  (`"ok"`/`"warn"`/`"down"`/`"hosting"`/`"idle"`) remain, all non-visible. ✓

## Deviations from plan
- Added a `hosting-enabled` `in property` + `host`/`stop` callbacks to `AppWindow` and a
  `host-state` field to `PortRow` — anticipated by the plan ("Add the `host`/`stop` callbacks +
  any new `in property`"); within the declared `ui/app-window.slint` file.
- Introduced `LiveState` / `rebuild_rows` / `derive_*` / `map_*` / `hosting_targets` helpers in
  `src/main.rs` (declared file) to keep derived state persistent across reloads — the plan's
  "preserve/derive the status field" requirement made caching the last load necessary.
- No files outside the declared list were touched.

## Surprises / notes
- The host engine `spawn` exists in both builds (no-op without `hosting`), so the toggle wiring
  compiles unconditionally; only the probe engine and `set_hosting_enabled(true)` are cfg-gated.
- Probe targets are only set for groups in confirmed `Hosting` state (engine event), so probing
  starts after the relay connects, not on the optimistic click.
- `ProbeCommand::SetInterval` is unused (dead-code warning in `probe.rs`); left for a future
  settings UI as the plan notes interval is code-configurable only.
