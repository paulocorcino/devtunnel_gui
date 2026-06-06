# Plan for #12: Auto-resume: re-host active groups on launch

## Feasible: yes
The issue is well-specified: persist the set of hosted group IDs to a JSON file
under `%APPDATA%/devtunnel-gui/`, update it on host/stop, and on startup re-send
`HostCommand::Host` for each previously-active group after the first
`fetch_rows`. All touch points (host/stop callbacks, startup pump, timer loop)
already exist in `src/main.rs`, and `serde`/`serde_json` are already deps. The
host engine from #4 (`host::HostCommand`) is in place.

## Execution model: sonnet
Localized change: one small persistence module plus a few wiring edits in
`main.rs` (two callbacks + a one-time post-first-load trigger in the existing
timer pump). The pattern mirrors existing code; no tricky lifetimes or new
concurrency primitives. The only subtlety (run-once after the first async load)
is a single boolean flag — well within Sonnet's range.

## Done when
- A new `src/config.rs` persists the auto-host group-ID set to
  `%APPDATA%/devtunnel-gui/auto-host.json`, with a save/load roundtrip covered
  by a unit test.
- Hosting a group adds its ID to the persisted set; stopping removes it.
- On startup, after the first successful `fetch_rows`, the app sends
  `HostCommand::Host` for each persisted group ID that still exists in the
  fetched rows (stale IDs are ignored).
- `cargo fmt` is clean and `cargo test` passes with no new warnings.

## Steps
- [x] Add `src/config.rs`: a small module with a `Config` (or
      `AutoHostStore`) holding a `HashSet<String>` of group IDs. Provide
      `load()` (reads `%APPDATA%/devtunnel-gui/auto-host.json` via
      `std::env::var("APPDATA")`, returns an empty set if the file is missing or
      unparseable) and `save()` (creates the dir if needed, writes pretty JSON).
      Use `serde`/`serde_json`; do not feature-gate it. Add `mod config;` to
      `src/main.rs`.
- [x] Add a unit test in `config.rs` for the serialize/deserialize roundtrip of
      the ID set (in-memory or a temp path — no reliance on the real APPDATA).
- [x] In `main`, load the persisted set once at startup into an
      `Rc<RefCell<HashSet<String>>>` (the "auto-host set"), shared with the
      host/stop callbacks and the pump.
- [x] In the `on_host` callback, insert the tunnel ID into the auto-host set and
      persist it (`config::save`).
- [x] In the `on_stop` callback, remove the tunnel ID from the auto-host set and
      persist it.
- [x] In the timer pump, add a run-once flag (e.g. `Cell<bool>` captured by the
      closure): the first time a load result is applied (`loaded == true`),
      iterate the persisted set and send `HostCommand::Host { tunnel_id }` for
      each ID present in `state.rows`, optimistically setting its host state to
      `"host"` and rebuilding rows (mirroring `on_host`). Ignore IDs no longer in
      the fetched rows.
- [x] Keep all new strings/comments in English; no user-facing string literals
      added (this change adds no new UI text).
- [x] cargo fmt && cargo test pass with no new warnings.
