---
id: "0004"
track: "t1"
status: "active"
related_adrs: ["ADR-0001", "ADR-0002"]
milestone: "PRD-0004: Hosting robustness"
---

# PRD-0004 — Hosting robustness (spike)

> Part of track `t1` · See [roadmap](../roadmap.md)
>
> **This is a spike: evaluate, do not build.** A "no-go with evidence" is a successful
> outcome for any slice. Production code is only written when a slice returns a "go".

## Problem / Why now

The Rust `tunnels` SDK lacks four capabilities the C#/TS SDKs have: automatic
reconnection, SSH-level reconnection, automatic token refresh, and SSH keep-alive. Two
that mattered are already reimplemented by hand and work: **reconnection** (manual
backoff loop in `src/host/engine.rs` → `host_group`) and **token refresh** (20h re-mint
timer, `REMINT_AFTER`). What remains is connection-robustness polish. The honest prior:
the in-process Rust bet (ADR-0001) was correct and ~80% of the value is harvested; the
remaining well is mostly dry. This spike gathers evidence that justifies or kills each
candidate rather than committing to build.

## Goals

- A documented go/no-go per candidate, backed by evidence.
- Where it costs almost nothing and removes a clear risk, land the deepening (e.g. testability of the keep-alive policy).

## Non-goals

- Shipping any production reconnect control loop, forking russh, or any zero-downtime token-refresh mechanism — unless a slice returns a "go" with evidence.
- Switching SDK language or adding a C#/TS sidecar for hosting (ADR-0001 deliberately chose in-process Rust; reintroducing multi-process IPC is not worth it).

## User / use case

A user hosting one or more Groups for hours/days across real network conditions
(sleep/wake, NAT idle, Wi-Fi switches), expecting the Public URL to keep serving
without manual Stop/Host.

## Requirements / expected behavior

The three candidates and their decision criteria:

| Item | Candidate | Go if… | Kill if… |
|---|---|---|---|
| 1 | Bridge probe→reconnect (#37 gate, #39 build) | zombie-tunnel state observed in real use AND a false-positive-safe trigger is feasible | no zombie state observed, or trigger cannot be told apart from a local-server restart |
| 2 | SSH keep-alive at the russh layer (#40) | a `keepalive_interval` knob is exposed upstream | no knob exists upstream → fork/upstream change, out of scope |
| 3 | Smooth the 20h re-mint (#38) | measured outage is user-perceptible (≳ a few seconds) | outage is sub-second / below perception |

Plus an orthogonal deepening (#35): extract the keep-alive policy into a pure,
testable state machine — zero behavior change — and (#36) honor the configured Port
protocol instead of hard-coding `http`.

## Definition of Done

This is a spike, so DoD is "every candidate has a recorded go/no-go", not "everything shipped":

- [ ] Item 2 resolved — **done: NO-GO** (#40, russh pins no keep-alive knob).
- [ ] Item 1 evidence gate (#37) run in real use; go/no-go recorded. Build (#39) only if go.
- [ ] Item 3 re-mint blip (#38) measured; go/no-go recorded. Make-before-break fix only if perceptible.
- [ ] Keep-alive state machine (#35) and port-protocol fix (#36) landed (or explicitly deferred).
- [ ] All issues in milestone `PRD-0004` are closed.

## Issue breakdown

| Issue | Title | Type | Status |
|-------|-------|------|--------|
| #35 | Keep-alive state machine extraction | AFK | open (needs-triage) |
| #36 | Port protocol from configuration | AFK | open (needs-triage) |
| #37 | Zombie-tunnel evidence gate | HITL | open (needs-info) |
| #38 | Measure the 20h re-mint blip | AFK | open (needs-info) |
| #39 | Probe→reconnect watchdog | AFK | open (needs-triage, blocked by #35 + #37) |
| #40 | russh keep-alive capability check | AFK | closed (wontfix — NO-GO) |

## Out of scope / explicitly rejected

- **Item 2 (SSH keep-alive)** — killed. russh 0.37.1 (pinned by `tunnels`) exposes no client keep-alive knob, and the session is owned inside the `tunnels` crate. Would require a fork/upstream change.
- Reintroducing a multi-process hosting model to harvest the SDK capability matrix (see ADR-0001).

## Links

- Roadmap: [docs/roadmap.md](../roadmap.md)
- Related ADRs: ADR-0001 (split CLI management / SDK hosting), ADR-0002 (per-group host thread isolation)
- Spike background: `docs/spikes/0001-sdk-hosting.md`
