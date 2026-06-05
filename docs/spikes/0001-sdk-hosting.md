# Spike #2 — Native hosting via `tunnels` SDK (HITL)

Validates the bet from [ADR-0001](../adr/0001-split-cli-management-sdk-hosting-rust.md):
the Rust `tunnels` SDK (feature `connections`) can host a tunnel **in-process**,
authorized by a host token minted via `devtunnel token`.

## Confirmed API (from SDK source)

- `new_tunnel_management("ua").into()` → `TunnelManagementClient` (default auth: `Anonymous`).
- `TunnelLocator::ID { cluster, id }` — the CLI `name.cluster` id splits at the last `.`.
- `RelayTunnelHost::new(locator, mgmt)` → `connect(host_token)` → `RelayHandle`
  (Future; completes on disconnect) → `add_port(&TunnelPort)`.
- `add_port` calls `create_tunnel_port` with anonymous auth but **treats 409 (already exists)
  as OK** → no management token needed if the port was already created via CLI.
- Host token: `devtunnel token <id> --scopes host -j` → `token` field.

## Risk findings

### 1. Heavy build toolchain on Windows (vendored OpenSSL)
The SDK pins `russh`/`russh-keys` with the **openssl** feature. To avoid depending on
a system OpenSSL, we use `vendored-openssl`, which compiles OpenSSL from source and requires:
- **NASM** (install via `winget install NASM.NASM`) — assembly fails without it.
- **Strawberry Perl** (install via `winget`) — the msys2 perl lacks
  `Locale::Maketext::Simple`, breaking OpenSSL's `./Configure`.
- **MSVC toolchain** (already present).

Implication: building from source on Windows is non-trivial. For *distribution* this
is a one-time dev cost (produces a ready binary), but it is a point against lightness.

### 2. Host token expires in ~24h
`devtunnel token --scopes host` returns `lifeTime: 1.00:00:01`. Long-running keep-alive
must **re-mint the token and reconnect** periodically (≤ 24h).

### 3. Hosting requires TWO tokens, not one
A single host token is **not enough**: `connect()` authorizes the relay endpoint with
the host token (ok), but `add_port()` calls `create_tunnel_port` on the mgmt client,
which with anonymous auth returns **401** (even if the port already exists — the 401
comes before the 409). Solution: two tunnel tokens:
- `host` → passed to `host.connect(host_token)`.
- `manage:ports` → default auth of the `TunnelManagementClient` (`add_port`).

### 4. CLI bug when repeating `--scopes`
`devtunnel token ... --scopes host --scopes manage:ports` corrupts the first scope to
`shost` (invalid). **Mint one scope per token** (separate calls).

## Runtime result — ✅ SUCCESS

```
relay connected ✓
port 3000 forwarded ✓
Public URL: https://9nfm43tl-3000.brs.devtunnels.ms/
curl → DEVTUNNEL_SPIKE_OK  (HTTP 200)
```

Public traffic reached the local server **without an external `devtunnel host` process**.
The SDK hosts in-process. **ADR-0001 confirmed.**

## Recommendation

Proceed with SDK hosting (ADR-0001). Encapsulate behind a `TunnelHost` trait
(SDK implementation), keeping the escape hatch for `devtunnel host` fallback only if
maintenance (token refresh / reconnect / OpenSSL build) proves too costly.

### Implications for upcoming slices
- **#4 (hosting):** mint 2 tokens per group (`host` + `manage:ports`); reconnect and
  re-mint before ~24h; "stop" = drop the `RelayHandle`.
- **Build/CI:** document/automate NASM + Strawberry Perl + `vendored-openssl`
  (or produce a pre-compiled binary for the end user).
