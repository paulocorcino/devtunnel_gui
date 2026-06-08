# Onboarding readiness overhaul: gate actions, actionable banner, install feedback, guided first-run

Status: needs-triage

## Parent

[docs/backlogs/onboarding-first-run/PRD.md](../PRD.md)

## What to build

Make first-run completable by a novice on a clean machine (no CLI, not signed in)
without hitting dead ends or error-on-click traps. One end-to-end pass across the
preflight states (`cli-missing` → `relogin` → `ready`), touching the Slint UI, the
Rust handlers, and the i18n strings. The sign-in regression (hidden interactive
`user login`) is already fixed and is out of scope here.

Four behaviors land together:

1. **Gate actions on readiness.** When `app-state != "ready"`, Add port, Refresh,
   and the empty-state "+ Create group" are disabled/hidden so no management call
   can fire into an "Unauthorized tunnel creation access" error.
2. **Actionable preflight banner.** `PreflightBanner` gains a primary action matched
   to the state — "Open Settings" / "Install CLI" when `cli-missing`, "Sign in"
   when `relogin` — reusing the existing `install-cli` / `sign-in` handlers.
3. **Install-CLI feedback.** The Install CLI button disables and shows "Installing…"
   while winget runs, then surfaces a clear success or a distinct failure (including
   the elevation-required case) instead of swallowing it.
4. **Guided first-run flow.** On first launch, show a step sequence
   (CLI installed → signed in → ready) in place of the empty grid + Settings
   checklist, auto-advancing as preflight clears each gate. Includes a design/layout
   review and a clean-machine re-test to confirm it's still needed after 1–3.

## Acceptance criteria

- [ ] With CLI missing or not signed in, Add port / Refresh / "+ Create group" cannot be clicked into a management error (gated on `app-state`).
- [ ] The preflight banner offers a state-appropriate primary action that moves the user forward (Settings / Install CLI / Sign in), not the dead-end Create-group path.
- [ ] Clicking Install CLI shows in-UI progress and a clear final outcome; an elevation/privilege failure is surfaced, not silently swallowed.
- [ ] First launch presents a guided CLI → Sign in → ready sequence that advances automatically as each gate clears.
- [ ] All new UI text goes through the Fluent pipeline (`app.ftl` + `strings.slint`); no raw literals in Rust or Slint.
- [ ] Clean-machine walkthrough (no CLI, signed out) reaches a working tunnel unaided.
- [ ] `cargo build` and `cargo test` pass.

## Blocked by

None - can start immediately.

## Notes

- Internal build order: gate actions first (removes the error trap), then the banner
  action (adds the right path), then install feedback (independent), then the guided
  flow last — only if the first three don't already make first-run self-explanatory.
- Open question carried from the PRD: a token stored by an elevated console login
  lands in a different Windows profile than the non-elevated GUI; decide whether to
  detect and surface an identity mismatch. Needs a real-world repro before building.
