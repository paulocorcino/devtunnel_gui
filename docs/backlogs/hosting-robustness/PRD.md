# Spike: Hosting robustness — evaluate, do not build

Status: in-progress

## Spike results (so far)

| Item | Slice | Outcome |
|---|---|---|
| 2 — SSH keep-alive | 01 | **KILLED (`wontfix`).** russh 0.37.1 (pinned by `tunnels`) exposes no client keep-alive knob, and the session is owned inside the `tunnels` crate. Would require a fork/upstream change → out of scope per criteria. |
| 1 — probe→reconnect | 02 | **Open (`needs-info`).** Pure HITL: needs a real-world observation window; cannot be concluded headless. Importance raised because item 2 is dead — this is now the only half-open detection path. |
| 3 — re-mint smoothing | 03 | **Open (`needs-info`), hypothesis reversed.** Static analysis shows the re-mint does a full reconnect fronted by **two sequential `devtunnel.exe` token mints** (network round-trips) before the relay is back — plausibly several seconds, not sub-second. Cheap make-before-break fix identified. Needs live measurement to confirm a go. |

Net: the only candidate with a positive prior (item 2) is dead. Items 1 and 3 both
depend on a live hosting run + observation that is not possible headless. No
production code was written.

## Why this is a spike, not a feature

The Rust `tunnels` SDK lacks four capabilities the C#/TS SDKs have: automatic
reconnection, SSH-level reconnection, automatic token refresh, and SSH keep-alive
(see the SDK capability matrix in `microsoft/dev-tunnels`). Two of those that
actually mattered are **already reimplemented by hand** and work:

- **Reconnection** — manual backoff loop in `src/host/engine.rs` (`host_group`).
- **Token refresh** — 20h re-mint timer in the same file (`REMINT_AFTER`).

What remains is connection-robustness polish. The honest prior is that the
remaining well is mostly dry: the in-process Rust bet was correct and ~80% of the
expected value is harvested. This spike exists to **gather evidence that justifies
or kills** each of three candidate improvements — not to commit to building them.

## Explicit bias

We enter this spike expecting **only item 2 to survive**, and only if it is nearly
free. Items 1 and 3 require real evidence to proceed; absent that, building them is
speculative and is the overengineering we are explicitly avoiding. A "no-go with
evidence" is a successful outcome for any slice.

## Candidates under evaluation

1. **Bridge probe→reconnect.** When the health probe reports `Down` for a
   *should-be-hosting* group, force a reconnect. Premise: a silent half-open relay
   can leave a zombie tunnel that `RelayHandle` never detects, so nothing
   reconnects. Risk: false positives (a local server restart also reads as `Down`)
   and flapping/races with the SDK's own drop detection.

2. **SSH keep-alive at the russh layer.** If `russh`/`tunnels` exposes a
   `keepalive_interval` knob, enabling it gives proactive half-open detection for
   nearly zero cost. If the knob does not exist, the fix becomes a fork/upstream
   contribution and is **out of scope**.

3. **Smooth the 20h re-mint.** Today the token re-mint drops the relay and does a
   full reconnect (re-mint two tokens, rebuild client, re-add ports). True
   zero-downtime refresh requires SDK support Rust lacks. Hypothesis: the resulting
   outage is sub-second and below user perception, making this not worth doing.

## Scope

- **In:** investigation, instrumentation, measurement, and a documented go/no-go
  per candidate.
- **Out:** shipping any production reconnect control loop, forking russh, or any
  zero-downtime token-refresh mechanism. Those become separate issues only if a
  slice returns a "go" with evidence.

## Decision criteria

| Item | Go if… | Kill if… |
|---|---|---|
| 1 — probe→reconnect | zombie-tunnel state is observed in real use AND a false-positive-safe trigger is feasible | no zombie state observed, or trigger cannot be distinguished from a local-server restart |
| 2 — russh keep-alive | a `keepalive_interval` (or equivalent) knob is exposed | no knob exists upstream |
| 3 — re-mint smoothing | measured outage is user-perceptible (≳ a few seconds, not absorbed by client retry) | outage is sub-second / below perception |

## Slices

- `issues/01-russh-keepalive-capability-check.md` (AFK) — cheapest, most decisive.
- `issues/02-zombie-tunnel-evidence-gate.md` (HITL) — needs real-world observation.
- `issues/03-measure-remint-blip.md` (AFK) — expected to kill item 3.

## Out of scope for the whole front

Switching SDK language or adding a C#/TS sidecar for hosting. ADR-0001 deliberately
chose in-process Rust hosting; reintroducing a multi-process model to harvest the
SDK matrix is not worth the runtime weight and IPC cost given items 1–3 cover the
real gaps.
