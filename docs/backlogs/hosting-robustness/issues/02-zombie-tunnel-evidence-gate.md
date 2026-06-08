# 02 — Zombie-tunnel evidence gate

Status: needs-info
Type: HITL
Resolution: pending real-world observation (cannot be concluded headless).

## Parent

[Spike: Hosting robustness](../PRD.md)

## What to build

Evidence gate for the probe→reconnect bridge (item 1). The bridge's premise is that
a silent half-open relay can leave a **zombie tunnel**: the public URL is dead, the
probe reports `Down`, but the SDK's `RelayHandle` never resolves, so the host engine
never reconnects.

Instrument enough to detect that state in real use, then observe over normal daily
hosting. HITL because the failure is rare and cannot be synthesized reliably — it
needs the app running across real network conditions (sleep/wake, NAT idle, Wi-Fi
switches) over days.

Do **not** build the reconnect control loop in this slice. If — and only if —
zombie state is observed, capture what a false-positive-safe trigger would look
like (must distinguish a dead tunnel from a local server merely restarting, since
both read as probe `Down`).

## Acceptance criteria

- [ ] Lightweight instrumentation added: log/flag when a group is `Down` per the probe while the host engine still believes it is `Hosting` (i.e. `RelayHandle` has not resolved).
- [ ] Observation window run in real use (suspend/resume, network changes) with the outcome recorded.
- [ ] Documented finding: was a zombie tunnel observed? How often, under what trigger?
- [ ] If observed: a sketch of a false-positive-safe reconnect trigger (how to tell a dead tunnel apart from a local-server restart).
- [ ] Go/no-go for item 1 recorded in `## Comments`. No-go (not observed) is a valid, successful outcome.

## Blocked by

None - can start immediately. (Independent of issue 01.)

## Comments

### Status note

This slice is genuinely HITL and cannot be concluded headless: the failure it hunts
(a silent half-open relay that `RelayHandle` never detects) is rare and only shows
up under real network conditions (suspend/resume, NAT idle, Wi-Fi switches) over
days of actual hosting. There is nothing to measure analytically.

**Raised importance after issue 01:** item 2 (SSH keep-alive) is killed (`wontfix`),
so there is no proactive half-open detection at the SSH layer. That makes this gate
the *only* path to deciding whether a probe→reconnect bridge is warranted. If, and
only if, the observation window surfaces a zombie tunnel does the bridge get built —
and its trigger must distinguish a dead tunnel from a local-server restart (both
read as probe `Down`).

### Remaining step to close

Add the lightweight instrumentation (log when a group is probe-`Down` while the host
engine still reports `Hosting`), then run the app in real daily use for the
observation window and record the outcome.
