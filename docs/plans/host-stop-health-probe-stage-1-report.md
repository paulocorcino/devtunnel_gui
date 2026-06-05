# Stage 1 — hosting feature + host module skeleton — Post-stage report

**Backlog items:** S1-feature, S1-mint, S1-trait, S1-stub
**Commit:** _filled by parent in the End-to-end summary table_
**Plan:** host-stop-health-probe.md

## Files changed
- `Cargo.toml` — added optional `ureq` dep (`default-features = false`, `tls`
  feature, rustls) and a new `hosting` feature
  (`["dep:tunnels", "dep:tokio", "dep:ureq", "dep:log"]`). The existing `spike`
  feature and the `host_spike` bin are untouched.
- `src/devtunnel.rs` — added `pub fn mint_token(full_id, scope, loc) -> Result<String>`
  (subprocess `devtunnel token <id> --scopes <scope> -j`, parses the `token`
  field) reusing the existing `bin()` + `err-cli-*` localized error strings; and
  `pub fn split_locator(full_id) -> Option<(String, String)>` returning
  `(cluster, id)` by splitting at the last `.`. Both carry a
  `#[cfg_attr(not(feature = "hosting"), allow(dead_code))]` so the default build
  is warning-free while Stage 2 (which consumes them) still sees the warning if
  it forgets to wire them.
- `src/host/mod.rs` (new) — `HostState`, `HostCommand`, `HostEvent` enums; the
  `TunnelHost` trait; a private `NoopHost` impl; and a cfg-gated `spawn(events)`.
  With `--features hosting` it returns a placeholder host (real engine lands in
  Stage 2); without it, a `NoopHost` that drops commands. Compiles both ways.
  Module-level `#![allow(dead_code)]` because the control surface is defined but
  not wired into the UI until Stage 4.
- `src/main.rs` — added `mod host;` next to `mod devtunnel;`. No callback wiring.

## Gate results
- `cargo build` (default, no feature): pass — compiles, `NoopHost` path active.
- `cargo build --features hosting` (PATH-prepended NASM + Strawberry Perl):
  pass — vendored OpenSSL + `tunnels` SDK + `ureq` compiled; binary built.
- `cargo test`: pass — 5 `sanitize_tunnel_id` tests still green.
- `cargo fmt -- src/host/mod.rs src/devtunnel.rs`: applied, files clean.
- `cargo clippy` (default): no warnings in the files I created/edited
  (pre-existing `needless_borrows_for_generic_args` warnings in `src/main.rs`
  are out of scope and left untouched).

## Acceptance criteria audit
- S1-feature: `hosting` cargo feature + optional `ureq` dep added; default build
  stays light (no SDK/OpenSSL pulled without the feature). ✓
- S1-mint: `mint_token` + `split_locator` added to `src/devtunnel.rs`, reusing
  `bin()` and `err-cli-*` strings. ✓
- S1-trait: `TunnelHost` trait + `HostState`/`HostCommand`/`HostEvent` types. ✓
- S1-stub: cfg-gated `spawn` (placeholder under `hosting`, `NoopHost`
  otherwise); module compiles both ways. ✓

## Deviations from plan
- The plan's Files note says to "Declare `engine` submodule with
  `#[cfg(feature = "hosting")] mod engine;`" but also says "no `engine.rs` yet"
  for Stage 1. Declaring the module without the file would break the `hosting`
  build, so the `mod engine;` declaration is deferred to Stage 2 (which creates
  `engine.rs`). A comment in `mod.rs` marks the seam. This matches the stage's
  own "keep all logic inline in `mod.rs`" instruction.
- Added `#[cfg_attr(not(feature = "hosting"), allow(dead_code))]` on the two new
  `devtunnel` helpers and `#![allow(dead_code)]` on the host module to keep the
  default build warning-free; these symbols are intentionally unused until
  Stages 2/4 wire them.

## Surprises / notes
- The `hosting` build still emits dead-code warnings for `mint_token` /
  `split_locator` (intentional — they have no caller until Stage 2). Stage 2
  should consume both; the warning is the reminder.
- `split_locator` returns `(cluster, id)` (vs the spike's `(id, cluster)` tuple
  order) to match the `TunnelLocator::ID { cluster, id }` shape the plan
  specifies for Stage 2.
