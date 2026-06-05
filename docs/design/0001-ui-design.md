# UI Design — DevTunnel GUI

Visual direction: **calm, status-forward SaaS dev tool** (ngrok / Tailscale / Linear
vibe). Compact, neutral, single accent, strong status colors. Pragmatic: no
illustrations, no heavy animations. The global `Theme`
([ui/theme.slint](../../ui/theme.slint)) is the single source of truth for
colors/spacing.

## Principles

1. **Status first.** The user opens the app to check "are my URLs up?". Each port has an always-visible status dot.
2. **URL is the product.** The Public URL is prominent (monospace font), copyable in 1 click.
3. **Group organizes, port is the unit.** List grouped by Group (=Tunnel); the actionable row is the port.
4. **Raw = bug.** No un-themed default widget. Everything goes through `Theme`.
5. **Dark mode is first-class** (devs work at night). Toggle via `Theme.dark`.

## Palette (`Theme`)

| Token | Light | Dark | Use |
|---|---|---|---|
| `bg` | `#ffffff` | `#0f1117` | window background |
| `surface` | `#f7f8fa` | `#171a21` | group cards, bars |
| `surface-2` | `#ffffff` | `#1e222b` | hovered row / inputs |
| `border` | `#e5e7eb` | `#262b36` | dividers, outlines |
| `text` | `#1f2330` | `#e6e8ee` | primary text |
| `muted` | `#6b7280` | `#9aa3b2` | captions, metadata |
| `accent` | `#4f46e5` | `#6366f1` | primary action, focus |
| `ok` | `#16a34a` | `#22c55e` | Operational |
| `warn` | `#d97706` | `#f59e0b` | Tunnel ok, service down |
| `down` | `#dc2626` | `#ef4444` | Down |
| `idle` | `#9ca3af` | `#9ca3af` | not hosted / stopped |

Metrics: `radius 8px` · `gap 8px` · `pad 12px` · mono font `Consolas` (URLs).
Typography: system UI (Segoe UI). Title 16–18 · section 13 semibold · body 12–13 ·
caption 11 muted.

## Status states (dot + label)

| State | Color | When | Source |
|---|---|---|---|
| Operational | `ok` | relay routing + local service responds | probe (#4/#5) |
| Service down | `warn` | relay responds, upstream dead (502/503) | probe |
| Down | `down` | URL unreachable | probe |
| Stopped | `idle` | group not hosted | local state |
| Hosting… | `accent` (light pulse) | connecting to relay | transition |

## Layout (mockup)

```
┌────────────────────────────────────────────────────────────────────┐
│ Dev Tunnels            ● Connected: paulo@…      [ Refresh ] [ ⚙ ] │  ← top bar (surface)
├────────────────────────────────────────────────────────────────────┤
│  ┌──────────────────────────────────────────────────────────────┐  │
│  │ frontend                       expires in 29d      [ ●○ Host ]│  │  ← group card header
│  │ ───────────────────────────────────────────────────────────── │  │
│  │  ● 3000  http   https://9nfm43tl-3000.brs.devtunnels.ms  ⧉  ↗ │  │  ← port row (status·port·proto·url·copy·open)
│  │  ● 5173  auto   https://9nfm43tl-5173.brs.devtunnels.ms  ⧉  ↗ │  │
│  └──────────────────────────────────────────────────────────────┘  │
│  ┌──────────────────────────────────────────────────────────────┐  │
│  │ apis                           expires in 12d      [ ○● Host ]│  │
│  │  ● 8049  http   https://…-8049.brs.devtunnels.ms         ⧉  ↗ │  │
│  └──────────────────────────────────────────────────────────────┘  │
│                                                       [ + New group ]│
└────────────────────────────────────────────────────────────────────┘
```

Empty state: centered card "No groups yet" + `+ Create group` button.

## Component inventory (Slint)

Files under `ui/`, consumed by `app-window.slint`:

- `theme.slint` — global `Theme` (✅ created).
- `StatusDot` — dot colored by state (+ tooltip with label).
- `Pill` — small chip (expiration, protocol, login status).
- `IconButton` — ghost button for row actions (copy ⧉, open ↗).
- `PrimaryButton` / `GhostButton` — filled / outline actions.
- `Toggle` — Host/Stop switch in the group card header.
- `GroupCard` — card: header (name, expiration, toggle) + `PortRow` list.
- `PortRow` — status · port · protocol · mono URL · hover actions.
- `Field` / `Select` / `Collapsible` — for dialogs (#3): port, group, "Advanced".
- `Toast` — ephemeral confirmation ("URL copied").

## Interactions

- **Click on URL** copies (toast "copied"). Explicit ⧉/↗ buttons also work.
- **Row actions** appear on hover (always visible on small screens).
- **Enter** confirms dialogs; **Esc** cancels.
- **Tray**: click opens/closes; Open/Quit menu (re-login state changes the icon — #5).
- Dark mode: follow Windows theme when feasible; otherwise toggle in ⚙.

## Slice mapping

| Slice | UI deliverable |
|---|---|
| #1 (done) | top bar + list; repainted with `Theme` (base of the system) |
| #3 | real `GroupCard`, dialogs with `Field/Select/Collapsible`, empty state, `+ New group` |
| #4 | `Toggle` Host, 3-state `StatusDot`, `Toast` |
| #5 | **re-login** state/icon in tray + login pill in top bar |
| #6 | ⚙ Settings screen (probe interval, auto-start, default expiration, dark mode) |

> Rule for upcoming slices: **no hard-coded colors or sizes** — always `Theme.*`.
> New components go in separate `ui/` files and are reused, not copy-pasted.
