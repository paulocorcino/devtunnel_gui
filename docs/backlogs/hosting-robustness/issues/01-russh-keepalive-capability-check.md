# 01 — russh keep-alive capability check

Status: wontfix
Type: AFK
Resolution: NO-GO — knob not exposed; item 2 killed per PRD.

## Parent

[Spike: Hosting robustness](../PRD.md)

## What to build

Investigation only. Determine whether the `tunnels` crate (feature `connections`)
or the underlying `russh` client exposes a periodic SSH keep-alive knob
(e.g. `keepalive_interval` / `keepalive_max` on the SSH client config) that the
host engine could enable on its relay connection in `src/host/engine.rs`.

This is the cheapest and most decisive slice: it resolves the only candidate we
expect to keep. The output is a binary answer plus, if the knob exists, the exact
config call site.

## Acceptance criteria

- [x] Documented answer: does `russh`/`tunnels` expose an SSH keep-alive knob reachable from our host code path? **No.**
- [x] If yes: the concrete config point identified. **N/A — no knob.**
- [x] If no: confirmation that enabling it would require a fork/upstream change — marks item 2 out of scope per the PRD. **Confirmed.**
- [x] Go/no-go recorded in a `## Comments` note on this issue. **No-go.**

## Blocked by

None - can start immediately.

## Comments

### Finding (no-go) — evidence

Dependency graph (from `Cargo.lock`): `tunnels` git `f9224e4`, transitively pinning
`russh 0.37.1`.

1. **russh 0.37.1 has no client-initiated keep-alive knob.** `russh::client::Config`
   fields are exactly: `client_id, limits, window_size, maximum_packet_size,
   preferred, connection_timeout, anonymous` (`russh-0.37.1/src/client/mod.rs`).
   There is no `keepalive_interval` / `keepalive_max`. The only `keepalive`
   references in russh 0.37.1 are receive-side (`client/encrypted.rs` *replies* to
   an incoming `keepalive@openssh.com`); nothing sends periodic keep-alives and
   there is no config to enable it. (Newer russh added `keepalive_*`, but it is
   pinned to 0.37.1 by `tunnels` and cannot be bumped independently.)

2. **The russh session is not reachable from our code.** `tunnels` builds the
   client `Config` and owns the session internally
   (`rs/src/connections/relay_tunnel_host.rs:367`, session held as
   `Arc<russh::client::Handle<Client>>`). The public surface we consume
   (`RelayTunnelHost` / `RelayHandle`) does not expose it, so even a manual
   keep-alive send would require forking `tunnels`.

**Conclusion:** the knob does not exist in the pinned russh, and the session is not
exposed — enabling SSH keep-alive requires an upstream/fork change. Per the PRD
decision criteria ("Kill if no knob exists upstream"), **item 2 is killed**. Set to
`wontfix`. The detection need it was meant to address is now wholly the
responsibility of the item-1 evidence gate (issue 02).
