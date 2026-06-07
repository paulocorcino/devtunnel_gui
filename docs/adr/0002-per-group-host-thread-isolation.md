# Per-group OS thread + runtime for the host engine

## Status

accepted — **confirmed** live (2026-06-07, issue #18): one app hosting 4 ports
across 3 tunnels (`fancy-ocean:3000`, `paulo-desktop:8049`, `frontend:3000+8049`)
served all Public URLs reliably under concurrent traffic, with no `HTTP 000` /
timeout starvation.

## Decision

In the SDK host engine (`src/host/engine.rs`, `--features hosting`), **each hosted
group runs on its own dedicated OS thread owning its own current-thread `tokio`
runtime + `LocalSet`**. The engine's command thread holds no async runtime of its
own — it is a lightweight synchronous dispatcher that, per `HostCommand::Host`,
spawns a group thread, and per `HostCommand::Stop`, signals that group's
cancellation `Notify` (which ends its `block_on`, dropping the runtime and
aborting that group's relay + forward tasks).

## Context and motivation

Stage 2 of issue #4 ran **all** host connections and their `forward_port_to_tcp`
tasks on a **single** current-thread runtime driven by one `LocalSet::block_on`.
That was chosen because the SDK's `RelayTunnelHost` / `russh` state is **`!Send`**,
so `spawn_local` (which requires a `LocalSet`) was the only way to drive the host
futures.

Issue #18 reported the consequence: hosting multiple groups with concurrent client
traffic stalled some forwards (`HTTP 000` / 12s timeout) while others on the same
machine kept working — one busy tunnel starving another because they shared one OS
thread.

## Considered Options

- **Single multi-threaded (work-stealing) `tokio` runtime.** Ruled out by the
  `!Send` constraint. A work-stealing runtime can only migrate **`Send`** futures
  between worker threads; an `!Send` host future must be pinned via `spawn_local`
  on a `LocalSet`, which fixes it to one thread. So a multi-threaded runtime would
  **not** distribute the host/forward load across cores and would **not** relieve
  the starvation — it would degrade to the same single-thread behavior for our
  tasks. This is the investigation the #18 proposal flagged; the conclusion
  confirms the issue's anticipated fallback.
- **Per-group OS thread + current-thread runtime (chosen).** Each group gets a
  real OS thread; the OS scheduler distributes them across cores, and no group can
  starve another's forwards. The `!Send` host future stays on its own thread, so
  `spawn_local` + `LocalSet` remain valid per group.

## Consequences

- No cross-tunnel head-of-line blocking: a heavy tunnel cannot stall another's
  port forwards; groups run with true parallelism across CPU cores.
- `Stop` is isolated: dropping one group's runtime tears down only that group's
  relay + forward tasks, never touching the others (replaces the old shared-runtime
  `handle.abort()`).
- One OS thread per hosted group instead of one shared thread. For the handful of
  groups a user hosts this thread/memory overhead is negligible and well worth the
  elimination of starvation.
- The command thread no longer needs a runtime or a std→tokio mpsc bridge; control
  flow is a plain synchronous dispatch loop.
