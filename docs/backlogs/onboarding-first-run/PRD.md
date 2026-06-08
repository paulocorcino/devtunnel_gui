# First-run onboarding: get a novice from launch to a working tunnel

Status: needs-triage

## Why

Field feedback on a clean machine (no CLI, not signed in) showed the first-run
path is a dead end for an inexperienced user. Each required step — install CLI,
sign in — either gives no feedback or actively misleads, and every action button
stays live and errors until the environment is ready. The cumulative effect is an
onboarding a non-expert cannot complete unaided.

The single hard **blocker** in that feedback — in-app Sign in could never complete
— is **already fixed** (see "Already shipped" below). This PRD covers the
surrounding UX gaps that remain, plus the harder "make first-run actually guided"
question.

## Already shipped (out of scope here)

**Sign-in regression.** Commit `51eb6eb` routed every subprocess through the
`CREATE_NO_WINDOW` builder, including the interactive `devtunnel user login`. With
the window hidden and stdio captured, the auth prompt was invisible and the app
sat on "signing in…" forever, forcing users to a console workaround (which, run as
Administrator, then stored the token in the wrong Windows profile so the GUI never
saw it). Fixed by splitting an `interactive_command` path (`CREATE_NEW_CONSOLE`,
inherited stdio, `.status()`) from the silent JSON path. `user_login` now runs in a
visible console. See `src/devtunnel.rs`.

## Problems observed (the remaining gaps)

| # | Observed | Root cause | File |
|---|---|---|---|
| A | With CLI missing / not signed in, "Add port", "Refresh", and **two** "+ Create group" buttons stay live and every one errors ("Unauthorized tunnel creation access"). | Action buttons and the empty-state CTA are not gated on readiness. | `ui/app-window.slint:200`, `ui/empty-state.slint`, top-bar buttons |
| B | The "Dev Tunnels CLI not found" / "Sign in required" banner is informational only — it tells the user what is wrong but offers no way forward. | `PreflightBanner` has no action; the path the user *can* click (Create group) is the wrong one. | `ui/app-window.slint:192-198` |
| C | "Install CLI" gives no progress; clicked repeatedly with no signal; a privilege/elevation failure is swallowed. | `on_install_cli` spawns winget with no in-UI state and no failure surfacing. | `src/main.rs:345-367` |
| D | The Settings "Requirements" checklist is the only place readiness is explained — a novice has to decode it rather than be guided. | No first-run guided flow; readiness lives behind the Settings popover. | `ui/settings.slint:49-155` |

## Scope

- **In:** gating actions on readiness; an actionable banner; Install-CLI progress
  and failure feedback; a guided first-run flow (CLI → Sign in → ready) that
  replaces "decode the checklist" for new users.
- **Out:** changing the auth mechanism itself (browser/device-code login via the
  CLI stays); non-Windows onboarding; any change to the hosting engine.

## Candidate slices (to be split into `issues/` on triage)

1. **Gate actions on readiness.** Disable/hide Add port, Refresh, and the
   empty-state "+ Create group" when `app-state != "ready"`. Removes the
   error-on-click trap (Problem A). Cheapest, fully AFK, high value.
2. **Actionable preflight banner.** Give `PreflightBanner` a primary action that
   matches the state: "Open Settings" / "Install CLI" when `cli-missing`, "Sign in"
   when `relogin` (Problem B). Depends on 1 for the "what should the user do next"
   answer.
3. **Install-CLI feedback.** Disable the button and show an "Installing…" status
   while winget runs; surface success and failure (including the elevation-required
   case) instead of swallowing them (Problem C).
4. **Guided first-run flow.** A step sequence (CLI installed → signed in → ready)
   shown on first launch instead of the empty grid + checklist, advancing
   automatically as preflight clears each gate (Problem D). Largest; do last, and
   only if 1–3 don't already make first-run self-explanatory.

## Decision criteria

| Slice | Go if… | Kill / defer if… |
|---|---|---|
| 1 gate actions | trivial and removes the error trap | n/a — clearly worth doing |
| 2 actionable banner | each non-ready state has one obvious next action | the action duplicates a Settings affordance with no added clarity |
| 3 install feedback | winget failure modes (esp. elevation) are observable and worth surfacing distinctly | winget already elevates/feeds back acceptably in practice |
| 4 guided flow | slices 1–3 still leave a novice stuck (re-test on a clean machine) | 1–3 already make first-run completable unaided — then this is overbuild |

## Open question

Token-profile mismatch: a user who signs in via an elevated console stores the
token where the non-elevated GUI can't read it. The in-app login fix sidesteps
this for the happy path, but should the app detect "CLI reports a different
logged-in identity than expected" and say so? Needs a real-world repro before
committing — flagged here so it isn't lost.
