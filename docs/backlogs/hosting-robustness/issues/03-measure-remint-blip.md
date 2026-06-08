# 03 — Measure the 20h re-mint blip

Status: needs-info
Type: AFK
Resolution: pending live measurement. Static analysis already reverses the PRD hypothesis (see Comments).

## Parent

[Spike: Hosting robustness](../PRD.md)

## What to build

Measurement only, for the re-mint smoothing candidate (item 3). Today the periodic
token re-mint (`REMINT_AFTER` in `src/host/engine.rs`) drops the relay and performs
a full reconnect: re-mint two tokens, rebuild the management client, reconnect the
relay, and re-add every port. Quantify the resulting outage.

In a throwaway test build, lower `REMINT_AFTER` to seconds and time the gap from the
relay drop to the next `Hosting` state (and, ideally, the window during which the
public URL does not serve traffic). The hypothesis is that the outage is sub-second
and absorbed by normal HTTP client retry — which kills item 3.

## Acceptance criteria

- [ ] Measured duration of the re-mint outage (drop → `Hosting`), repeated a few times for a stable figure.
- [ ] Measured (or reasoned) window during which the public URL fails to serve.
- [ ] Finding compared against the PRD threshold (perceptible ≳ a few seconds vs. absorbed by retry).
- [ ] Go/no-go for item 3 recorded in `## Comments`. Sub-second result = no-go (kill), as hypothesized.

## Blocked by

None - can start immediately.

## Comments

### Static-analysis pre-finding (hypothesis likely WRONG)

I cannot produce the measured numbers headless: it needs a `--features hosting`
build (NASM/Strawberry-Perl/vendored-OpenSSL toolchain) plus an interactive
`devtunnel user login` and a live tunnel. The boxes above stay unchecked until that
run happens. **But** reading the re-mint path changes the prior:

The re-mint does a **full** reconnect via `connect_once` in `src/host/engine.rs`,
and the outage is front-loaded with blocking work *before* the relay is back:

1. `devtunnel::mint_token(host)` — blocking `devtunnel.exe` subprocess + network
   round-trip (`src/devtunnel.rs:384`).
2. `devtunnel::mint_token(manage:ports)` — a **second** sequential subprocess +
   network round-trip.
3. `RelayTunnelHost::connect` — relay TLS/SSH handshake.
4. `add_port` for every port.

Two sequential process spawns each doing a network call, then a relay handshake,
is plausibly **several seconds**, not sub-second. So the PRD's "sub-second → kill"
hypothesis is probably false; the blip may be perceptible (though once per ~20h and
usually absorbed by HTTP client retry).

**Cheap fix identified (if measurement confirms it's perceptible):** make-before-break.
Mint both tokens *while the current relay is still up* (no outage during the two
subprocess calls), build the new connection, then drop the old handle — collapsing
the outage to just the relay handshake. No SDK support required. This is the actual
"smooth the re-mint" work and stays out of scope until the measurement gives a go.

### Remaining step to close

Throwaway build with `REMINT_AFTER` lowered to seconds; time drop→`Hosting` a few
times; compare against the perceptible threshold. Requires a human (interactive
login).
