# Blackbox E2E resilience suite

Exercises DevTunnel GUI **as a product**: it creates groups (tunnels) on a
shared local port, hosts them through the *production* keep-alive engine running
headless, serves a real Python backend, hammers the public URLs, and runs
resilience scenarios while sampling the host process. The goal is **stability
and efficiency**, not usability.

## How it drives the real engine

The GUI tray app can't be scripted, but its hosting engine is the product's
value. `src/headless.rs` adds a headless entrypoint: when
`DEVTUNNEL_HEADLESS_HOST=<id>[,<id>…]` is set, the binary drives the exact
production path (`host::spawn` → `engine::host_group` → the keep-alive state
machine) instead of building any UI, and streams every `HostEvent` as one JSON
line on stdout. The harness reads that stream and sends `stop <id>` / `host <id>`
/ `quit` on stdin. So the suite measures the real connect / keep-alive /
reconnect code, observed purely from the outside.

## Prerequisites

- `devtunnel` CLI signed in: `devtunnel user login`
- Host binary built with the SDK engine:
  ```
  cargo build --features hosting
  ```
  (needs NASM + Strawberry Perl + MSVC on PATH — see `CLAUDE.md`).
- Python deps: `pip install -r tests/e2e/requirements.txt`

## Run

```
python tests/e2e/run_e2e.py --groups 2 --port 3000 --load-secs 45
```

Writes `tests/e2e/report.md` and prints a live summary. Created tunnels use the
`e2e-*` prefix and are deleted on teardown.

## Scenarios

| id | what it proves |
|----|----------------|
| S2 | N tunnels on one local port all forward independently (no starvation) |
| S3 | throughput, p50/p95/p99 latency, error rate, **idle + loaded host CPU/RSS** (catches the relay busy-loop regression) |
| S1 | reconnect after a drop — stop→rehost proxy always; a real relay drop via firewall block only when run elevated |
| S4 | auto-resume — kill the host process, relaunch, recover serving |

## Limitations

- A genuine relay drop (S1b) blocks the host binary's outbound traffic with a
  Windows Firewall rule, which needs an **elevated** shell. Without it the suite
  uses the stop→rehost proxy and says so in the report.
- The headless runner re-hosts only the ids it's given; GUI auto-resume (which
  re-hosts the previously-active set) is approximated by S4's process kill.
