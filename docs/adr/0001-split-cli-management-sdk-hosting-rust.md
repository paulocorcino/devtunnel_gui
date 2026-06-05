# Rust + CLI for management, native SDK for hosting

## Status
accepted — **confirmed** by spike #2 (2026-06-05): the SDK hosts in-process
end-to-end (see [docs/spikes/0001-sdk-hosting.md](../spikes/0001-sdk-hosting.md)).
Hosting is placed behind a `TunnelHost` trait, leaving `devtunnel host` as a cheap
fallback if maintenance (token refresh / OpenSSL build) becomes too costly.

## Decision

The app is written in **Rust** and splits the Microsoft Dev Tunnels integration into
two layers: **account management** operations (create/list/delete/port/access/
update/renew) are done by calling the `devtunnel.exe` binary as a **one-shot
subprocess** (`std::process::Command`, with `-j/--json` output); **long-running
hosting** (keep-alive) uses the **official Rust `tunnels` SDK** (feature
`connections`) running **in-process**, authorized by a host-scoped token minted with
`devtunnel token <id> --scopes host`.

## Context and motivation

The goal is a tray app that keeps public URLs reliably alive, with an experience close
to paid solutions, without PowerShell and without over-engineering. The pain of the
current script (PowerShell) is hosting: launching `devtunnel host` as an external
process and scraping the URL from stdout, one process per port.

## Considered Options

- **Go:** ruled out — the official Go SDK has Management API but **no** Tunnel Host
  Connections; it cannot host in-process, which is the key benefit.
- **Pure native SDK (including auth):** more elegant, but the Dev Tunnels service is
  Microsoft first-party; obtaining a user (account-level) token would require either
  reading an undocumented keychain cache, or self-registering an OAuth app whose
  first-party scope may **not be granted**. High risk for the common case (create/list).
- **Pure CLI/PowerShell (status quo):** keeps the pain of N long-running `devtunnel host`
  processes + URL scraping; this is what we are moving away from.

## Consequences

- Native (SDK) code is concentrated where it delivers value: in-process hosting, one
  host connection per group, no stdout scraping.
- Depends on `devtunnel.exe` installed and logged in. The CLI login token is valid for
  only a few days → there is a natural keep-alive ceiling; the app handles this with an
  explicit **re-login** state (see CONTEXT.md).
- The `tunnels` SDK is v0.1.0 (preview, active in 2026) — maturity risk in the hosting
  layer; the hosting layer must stay isolated behind an interface to allow fallback to
  `devtunnel host` if needed.
- Host-scoped token has a limited lifetime; long-running keep-alive may require periodic
  re-minting + reconnect.
