---
id: "0003"
track: "t0"
status: "done"
related_adrs: []
milestone: "PRD-0003: First-run onboarding"
---

# PRD-0003 — First-run onboarding

> Part of track `t0` · See [roadmap](../roadmap.md)

## Problem / Why now

Field feedback on a clean machine (no CLI, not signed in) showed the first-run path
was a dead end for an inexperienced user. Each required step — install CLI, sign in —
either gave no feedback or actively misled, and every action button stayed live and
errored until the environment was ready. The cumulative effect was an onboarding a
non-expert could not complete unaided.

The single hard **blocker** (in-app Sign in could never complete) was fixed
separately: commit `51eb6eb` had routed the interactive `devtunnel user login` through
`CREATE_NO_WINDOW`, hiding the auth prompt forever. Fixed by splitting an
`interactive_command` path (`CREATE_NEW_CONSOLE`, inherited stdio, `.status()`) from
the silent JSON path. This PRD covered the surrounding UX gaps.

## Goals

- Gate management actions on readiness so no button errors into "Unauthorized tunnel creation access".
- Give the preflight banner a state-appropriate primary action.
- Surface Install-CLI progress and failure (including the elevation case).
- Guide first launch (CLI → Sign in → ready) instead of the empty grid + Settings checklist.

## Non-goals

- Changing the auth mechanism itself (browser/device-code login via the CLI stays).
- Non-Windows onboarding.
- Any change to the hosting engine.

## User / use case

A non-expert on a clean Windows machine launching the app for the first time, with no
Dev Tunnels CLI installed and not signed in.

## Requirements / expected behavior

- With CLI missing or not signed in, Add port / Refresh / "+ Create group" cannot be clicked into a management error (gated on `app-state`).
- The preflight banner offers a state-appropriate primary action (Settings / Install CLI / Sign in), not the dead-end Create-group path.
- Install CLI shows in-UI progress and a clear final outcome; an elevation/privilege failure is surfaced, not swallowed.
- First launch presents a guided CLI → Sign in → ready sequence that advances automatically as each gate clears.
- All new UI text goes through the Fluent pipeline (`app.ftl` + `strings.slint`).

## Definition of Done

- [x] Clean-machine walkthrough (no CLI, signed out) reaches a working tunnel unaided.
- [x] `cargo build` and `cargo test` pass.
- [x] All issues in milestone `PRD-0003` are closed.

## Issue breakdown

| Issue | Title | Type | Status |
|-------|-------|------|--------|
| #34 | Onboarding readiness overhaul: gate actions, actionable banner, install feedback, guided first-run | AFK | closed |

## Out of scope / explicitly rejected

- The token-profile mismatch open question (a token stored by an elevated console login lands in a different Windows profile than the non-elevated GUI). The in-app login fix sidesteps it for the happy path; detecting and surfacing an identity mismatch needs a real-world repro before committing — not in this PRD.

## Links

- Roadmap: [docs/roadmap.md](../roadmap.md)
- GitHub issue: #34 (closed)
