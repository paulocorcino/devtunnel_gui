# DevTunnel GUI — a desktop tray app for Microsoft Dev Tunnels on Windows

> A simple, native Windows tray application to create, host, and monitor
> **Microsoft Dev Tunnels** — expose a local port to a public HTTPS URL without
> the command line. Think "ngrok-style UX", but built entirely on Microsoft's
> own, free Dev Tunnels service.

<p align="center">
  <em>Manage your dev tunnels from the system tray — create groups, add ports,
  host them, and watch their health, all without memorizing CLI flags.</em>
</p>

<p align="center">
  <img alt="Platform: Windows" src="https://img.shields.io/badge/platform-Windows-blue">
  <img alt="Built with Rust" src="https://img.shields.io/badge/built%20with-Rust-orange">
  <img alt="UI: Slint" src="https://img.shields.io/badge/UI-Slint-2379c2">
  <img alt="License: MIT" src="https://img.shields.io/badge/license-MIT-green">
  <img alt="Uses Microsoft Dev Tunnels" src="https://img.shields.io/badge/powered%20by-Microsoft%20Dev%20Tunnels-5E5E5E">
</p>

---

## What is this?

**DevTunnel GUI** is a lightweight desktop app for Windows that gives
[Microsoft Dev Tunnels](https://learn.microsoft.com/azure/developer/dev-tunnels/overview)
a friendly graphical interface. Dev Tunnels lets you take a service running on
your machine (say, a web app on `localhost:3000`) and reach it from anywhere
through a secure public URL — useful for sharing a work-in-progress, testing
webhooks, demoing to a client, or debugging on a mobile device.

The official Dev Tunnels experience is a command-line tool (`devtunnel.exe`).
This app wraps that workflow in a tray icon and a clean window so you can:

- spin up tunnels and ports with a couple of clicks,
- keep them alive in the background, and
- see at a glance whether each one is actually reachable.

> [!IMPORTANT]
> **This project uses only Microsoft's own resources.** It is a thin,
> independent client on top of the official Microsoft Dev Tunnels service and
> the official `devtunnel` CLI / SDK. There is **no third-party tunneling
> server, no proxy, and no account of ours** in the middle. Your traffic flows
> through Microsoft's infrastructure under **your own** Microsoft / Entra ID /
> GitHub account — exactly as it would if you used the CLI directly.
>
> This is **not** an official Microsoft product and is not affiliated with or
> endorsed by Microsoft.

---

## Features

- **Groups = tunnels.** Organize your work into named groups (e.g. `frontend`,
  `apis`). Each group maps 1:1 to a Dev Tunnel and can hold multiple ports.
- **One-keystroke ports.** Add a port by typing its number and pressing Enter;
  pick the protocol (`auto` / `http` / `https`) when you need to.
- **In-process hosting.** Keep tunnels alive directly inside the app (via the
  official `tunnels` SDK) — no separate `devtunnel host` window to babysit.
- **Live health probe.** A periodic check classifies every hosted group as
  **Operational**, **Service down** (tunnel up, your local app isn't), or
  **Down** (unreachable). Interval is configurable (default 60s).
- **Copy & open.** Copy the public URL or open it in your browser from the
  window or the tray menu.
- **Auto-start & auto-resume.** Optionally start with Windows (minimized to
  tray) and re-host the groups that were active before you last quit.
- **Expiration auto-renewal.** Renews the tunnel expiration and reapplies its
  access rules so long-lived tunnels don't silently expire.
- **Re-login flow.** When your Microsoft sign-in expires, the app shows a tray
  alert and a one-click **Sign in** button instead of failing silently.
- **Dark mode & i18n.** Follows the Windows light/dark setting; all UI text
  goes through a Fluent localization pipeline (English included).

---

## Requirements

| Requirement | Details |
|---|---|
| **OS** | Windows 10 / 11 (64-bit). The tray, notifications, and auto-start are Windows-specific. |
| **Microsoft Dev Tunnels CLI** | The `devtunnel` executable must be on your `PATH`. Install it with: `winget install Microsoft.devtunnel` (or set the `DEVTUNNEL_BIN` environment variable to its full path). |
| **A Microsoft account** | You sign in with a Microsoft, Microsoft Entra ID, or GitHub account — the same identity Dev Tunnels uses. Dev Tunnels is a **free** Microsoft service. |
| **Network** | Outbound HTTPS (port 443) to Microsoft's Dev Tunnels endpoints (`*.devtunnels.ms`, `*.rel.tunnels.api.visualstudio.com`) and sign-in domains. No inbound ports are opened on your machine. |

> First run with no CLI installed shows a banner telling you exactly how to
> install it — you won't be left guessing.

---

## Installation

### Option A — download a release

Grab the latest prebuilt `devtunnel_gui.exe` from the
[Releases](../../releases) page, then make sure the Dev Tunnels CLI is
installed:

```powershell
winget install Microsoft.devtunnel
```

Run the app, sign in when prompted, and you're ready.

### Option B — build from source

You need the Rust **MSVC** toolchain. The default GUI build needs nothing else:

```powershell
git clone <this-repo-url>
cd devtunnel_gui
cargo build --release --bin devtunnel_gui
```

The in-process hosting code lives behind the `hosting` / `spike` feature flags,
which pull the `tunnels` SDK (and a vendored OpenSSL build). Building those
features requires **NASM**, **Strawberry Perl**, and the MSVC toolchain on your
`PATH`. See [`docs/spikes/0001-sdk-hosting.md`](docs/spikes/0001-sdk-hosting.md)
for the full rationale and exact setup commands.

---

## Quick start

1. **Launch** the app — it lives in your system tray.
2. **New group** → name it (e.g. `frontend`). One group = one tunnel.
3. **Add port** → type the local port (e.g. `3000`) and press Enter.
4. **Host** the group. The app keeps it alive in the background.
5. **Copy** the public `https://…devtunnels.ms` URL and share or test it.
6. Watch the **health badge** to confirm it's reachable.

---

## ⚠️ Use at your own responsibility

Exposing a local service to the internet is powerful and inherently risky.
**You are solely responsible for what you expose and to whom.**

- This software is provided **"as is", without warranty of any kind** (see
  [LICENSE](LICENSE)). The authors are not liable for any data exposure,
  breach, abuse, downtime, or other damages arising from its use.
- **Only expose what you intend to expose.** A tunnel can make a local
  database admin panel, debug endpoint, or internal API reachable by anyone who
  has the URL.
- **Respect your organization's policies and applicable laws.** Some
  organizations restrict or forbid developer tunnels for security reasons, and
  can disable anonymous access at the policy level. Check before you tunnel
  corporate or client systems.
- Tunneling traffic transits **Microsoft's** Dev Tunnels infrastructure under
  your account. Review Microsoft's
  [Dev Tunnels security documentation](https://learn.microsoft.com/azure/developer/dev-tunnels/security)
  and terms before exposing anything sensitive.

---

## 🔒 Security recommendations

These reflect Microsoft's own guidance for Dev Tunnels — worth following
regardless of which client you use:

- **Prefer authenticated access.** By default, Dev Tunnels are **private** and
  only reachable by the account that created them. Keep it that way unless you
  have a specific reason not to.
- **Be deliberate about anonymous access.** Allowing anonymous access means
  *anyone on the internet who has (or guesses) the URL can reach your local
  server.* Microsoft's explicit advice: it's convenient for quick testing, but
  **don't use it with sensitive data.** This app keeps the choice visible in
  the new-group dialog — treat the toggle with respect.
- **Scope access instead of going public.** Where possible, share via your
  **Microsoft Entra tenant** (`--tenant`) or a specific **GitHub organization**
  (`--organization`) rather than fully anonymous.
- **Use short-lived tokens for non-interactive clients.** Dev Tunnels access
  tokens are scoped per-tunnel and **expire after ~24 hours**, which limits the
  blast radius of a leaked token. Don't paste tokens into public places.
- **Never expose secrets unintentionally.** Debug endpoints, `.env`-driven
  dev servers, admin consoles, and databases should not be tunneled unless you
  truly mean to. Bind sensitive local services to `127.0.0.1` and add app-level
  auth.
- **Stop tunnels when you're done.** An idle open tunnel is unnecessary attack
  surface. Use **Stop** (or quit the app) when a session is over.
- **Keep the CLI updated.** Security fixes land in the official `devtunnel`
  tool — keep it current via `winget upgrade Microsoft.devtunnel`.

> Note: Dev Tunnels enforces HTTPS (insecure connections are auto-upgraded),
> uses HSTS, requires TLS 1.2+, and shows an anti-phishing interstitial on
> first web access. Those protections come from Microsoft's service, not this
> app — but they're good reasons to rely on the official service rather than a
> homegrown proxy.

---

## How it works (architecture in one minute)

- **Management** (create / list / delete tunnels, ports, access, renewal) →
  one-shot calls to `devtunnel.exe` with JSON output. No PowerShell.
- **Hosting** (the long-running keep-alive) → the official Rust `tunnels` SDK
  running **in-process** on a background thread, authorized by a host-scoped
  token minted from the CLI.
- **UI** → [Slint](https://slint.dev) (declarative, Rust, single binary) with a
  tray icon; background threads talk to the UI via channels.
- **State** → the Dev Tunnels service is the source of truth; the app stores
  only your auto-host list and settings locally in `%APPDATA%\devtunnel-gui\`.

For the full domain model and design decisions, see
[CONTEXT.md](CONTEXT.md) and [`docs/adr/`](docs/adr/).

---

## FAQ

**Is Dev Tunnels free?** Yes — it's a free Microsoft developer service. This
app doesn't change that.

**Do you see my traffic or store my credentials?** No. There is no server of
ours. Sign-in is handled by the official CLI; tokens stay on your machine /
in Microsoft's keychain. Traffic goes through Microsoft's relays under your
account.

**Is this an official Microsoft app?** No. It's an independent open-source
client built on Microsoft's public service, CLI, and SDK.

**Does it work on macOS / Linux?** Not currently — it targets Windows (tray,
notifications, auto-start). The Dev Tunnels service itself is cross-platform.

---

## Contributing

Issues and PRDs live as markdown under [`docs/backlogs/`](docs/backlogs/) —
this repo uses a local, file-based tracker. Contributions are welcome; please
keep **all text in English** and route user-facing strings through the Fluent
i18n pipeline (see [CLAUDE.md](CLAUDE.md)).

## License

[MIT](LICENSE) © 2026 Paulo Corcino.

---

### Keywords

Microsoft Dev Tunnels · devtunnel · Windows tray app · localhost to public URL ·
expose local server · HTTPS tunnel · ngrok alternative · reverse tunnel ·
webhook testing · Rust · Slint · port forwarding GUI · secure dev tunneling.

> **Maintainer tip:** to maximize discoverability on GitHub, set the
> repository **About** description to a one-liner like
> *"Windows tray GUI for Microsoft Dev Tunnels — expose localhost to a secure
> public URL"* and add **Topics** such as:
> `dev-tunnels`, `devtunnel`, `microsoft`, `tunneling`, `localhost`,
> `port-forwarding`, `ngrok-alternative`, `windows`, `tray`, `rust`, `slint`,
> `webhooks`, `https-tunnel`, `developer-tools`.

---

## Sources & references

- [Microsoft — Dev Tunnels overview](https://learn.microsoft.com/azure/developer/dev-tunnels/overview)
- [Microsoft — Dev Tunnels security](https://learn.microsoft.com/azure/developer/dev-tunnels/security)
- [Microsoft — Dev Tunnels CLI reference](https://learn.microsoft.com/azure/developer/dev-tunnels/cli-commands)
- [Microsoft — Dev Tunnels FAQ](https://learn.microsoft.com/azure/developer/dev-tunnels/faq)
