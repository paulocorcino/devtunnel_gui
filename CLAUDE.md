# DevTunnel GUI — Agent Instructions

## Language

**All text in this project must be in English.** This applies to:

- Code comments (single-line and doc comments)
- Documentation files (`.md`)
- User-facing strings in the UI (labels, status messages, menu items, button text)
- Log messages and error strings
- Commit messages

Do not write Portuguese in any file. If you find Portuguese text while editing, translate it to English as part of the change.

## Project Overview

Desktop tray app (Windows) for managing Microsoft Dev Tunnels, targeting a UX close to paid solutions (ngrok/expose style). See `CONTEXT.md` for domain glossary and architecture decisions.

## Architecture

- **Management** (create/list/delete/port/access/update): one-shot subprocess calls to `devtunnel.exe` via `std::process::Command` with `-j/--json` output. Never use PowerShell.
- **Hosting** (keep-alive, long-running): Rust `tunnels` SDK (feature `connections`) running in-process, authorized by a host-scoped token from `devtunnel token <id> --scopes host`.
- **UI**: Slint declarative UI, tray via `tray-icon` crate.
- **Threading**: SDK (async/tokio) and CLI subprocess run on background threads; communicate with the Slint UI thread via channels.

## Key Rules

- No color or size hard-coded in UI components — always use `Theme.*`.
- New UI components go in `ui/` as separate files.
- The `spike` feature flag gates the SDK hosting code from the main GUI build.

## Building

- **Default GUI** (`cargo build` / `cargo build --bin devtunnel_gui`): no special tooling beyond the Rust MSVC toolchain.
- **Hosting / `--features spike`**: pulls the `tunnels` SDK, which builds `russh` + **vendored OpenSSL**. That requires, on `PATH`: **NASM**, **Strawberry Perl** (the msys2 perl lacks `Locale::Maketext::Simple`), and the MSVC toolchain. Rationale and install commands: `docs/spikes/0001-sdk-hosting.md`.
- On this machine, prepend to `PATH` before building the `spike` feature:
  `C:\Strawberry\perl\bin`, `C:\Strawberry\c\bin`, `C:\Users\PICHAU\AppData\Local\bin\NASM`.

## i18n

All user-facing strings go through the Fluent (`fluent-bundle`) pipeline — never hardcode UI text in Rust or Slint.

### Where strings live
- **Locale files**: `i18n/en-US/app.ftl` (and future `i18n/<tag>/app.ftl`). Add every new string here first.
- **Slint global**: `ui/strings.slint` — one `in property` per string. Default values are English fallbacks so the UI renders before the locale loads.
- **Rust wrapper**: `src/locale.rs` — `Locale::load(lang)`, `loc.t("key")`, `loc.t_args("key", &args)`.

### Rules to avoid regression
1. **No raw string literals for UI text** in `src/main.rs`, `src/devtunnel.rs`, or any `.slint` file. Every visible string must have an entry in `app.ftl`.
2. **Slint components bind to `Strings.*`**, never to a string literal. Example: `text: Strings.btn-refresh` — not `text: "Refresh"`.
3. **Parametric strings use `FluentArgs`** — do not use `format!()` for user-visible messages; use `loc.t_args("key", &args)` with named variables declared in the FTL.
4. **`apply_strings(&app, &loc)` must be called** immediately after `AppWindow::new()`, before the event loop starts. If you add a new `Strings` property, add the matching `s.set_*()` call there.
5. **Adding a new locale**: create `i18n/<tag>/app.ftl` with all keys present, then add a `match` arm in `locale::ftl_source()`. Missing keys fall back silently in release but panic in debug — run `cargo test` (or `cargo check`) to catch them early.
6. **`sys-locale` detects the system locale**; override with `DEVTUNNEL_LANG=<tag>` for testing.

## Skills

### Rust (`/rust-skills`)

This is a Rust project. When writing, reviewing, or refactoring Rust code, invoke the `rust-skills` skill for best-practice guidance. It covers 179 rules across 14 categories: ownership & borrowing, error handling, async patterns, API design, memory optimization, performance, testing, and common anti-patterns.

## Agent skills

### Issue tracker

Issues are tracked on **GitHub** (`paulocorcino/devtunnel_gui`) via the `gh` CLI. PRDs are versioned in-repo under `docs/prd/`, one per roadmap track, each mapped to a GitHub Milestone. See `docs/agents/issue-tracker.md`. (`docs/backlogs/` is archived — superseded by this model; see its README.)

### Triage labels

Five canonical roles mapped to this repo's GitHub labels: `needs-triage`, `needs-info`, `ready-for-agent` → `AFK`, `ready-for-human` → `HITL`, `wontfix`. See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: one `CONTEXT.md` + `docs/adr/` at the repo root. See `docs/agents/domain.md`.

### PRD / roadmap track model

Adopted. `docs/roadmap.md` is the thin portfolio of tracks; each track is backed by a PRD in `docs/prd/NNNN-tN-slug.md` that maps to one GitHub Milestone. `/to-prd` writes the PRD; `/to-issues` opens the milestone's issues.
