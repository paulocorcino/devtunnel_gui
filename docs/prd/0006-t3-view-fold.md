---
id: "0006"
track: "t3"
status: "active"
related_adrs: []
milestone: "PRD-0006: Pure view-fold module"
---

# PRD-0006 — Pure view-fold module

> Part of track `t3` · See [roadmap](../roadmap.md)

## Problem / Why now

The answer to "why does this Port show this badge?" is smeared across `src/main.rs`:
`rebuild_rows` (~127 lines, 5-level nesting), `apply_rows`, `derive_status`, and the
`LiveState` maps. Four independent sources of truth — CLI rows, Health probe results,
Host state, and the optimistic-delete hidden set — are merged with implicit invariants.
A desync is silent, and none of it is tested. This is the highest-leverage piece of
untested glue in the app.

## Goals

- Extract view reconciliation into a pure module (e.g. `src/view.rs`) with one entry point returning plain data — no Slint types, channels, or `Rc<RefCell>` in the core.
- Make badge derivation, optimistic-delete hiding, placeholder folding, and host-state mapping unit-testable.
- Zero behavior change.

## Non-goals

- Any visible UI change — same grouping, badges, hiding, placeholders, detail-panel selection.
- Changing the probe, host engine, or CLI layers.

## User / use case

Internal: maintainers and agents reasoning about and testing the view layer. No
end-user-visible change.

## Requirements / expected behavior

- A pure entry point, roughly `fn fold(rows, probe, host, hidden, placeholders) -> Vec<GroupView>`, returning plain data; a thin mapping to the Slint models stays in main.rs.
- The tray-menu rebuild is derived from the fold's output, not folded inline.
- Same grouping, badge derivation (probe → ok/warn/down), optimistic-delete hiding, placeholder rows, and detail-panel selection behavior.

## Definition of Done

- [ ] `rebuild_rows`/`derive_status` logic lives in a pure module with no Slint, channel, or `Rc<RefCell>` dependencies.
- [ ] Table-driven unit tests cover: 3 probe-state badge mapping, optimistic-delete hiding (hidden port skipped, group with all ports hidden), placeholder folding, host state → hosting pill.
- [ ] main.rs only feeds inputs and pushes outputs to Slint + tray; no folding logic inline.
- [ ] Default `cargo build` and `cargo build --features hosting` both pass; UI renders identically.
- [ ] All issues in milestone `PRD-0006` are closed.

## Issue breakdown

| Issue | Title | Type | Status |
|-------|-------|------|--------|
| #42 | Pure view-fold module (extract reconciliation from main.rs) | AFK | open (needs-triage) |

## Out of scope / explicitly rejected

- Any behavior change to the view — this is a pure refactor for testability.

## Links

- Roadmap: [docs/roadmap.md](../roadmap.md)
