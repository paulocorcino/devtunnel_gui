# Ralph runner — autonomous overnight issue worker (Windows)

Works GitHub issues labelled **`AFK`** unattended, **one PR per issue**, on your
Claude **subscription quota** (no Anthropic API key, so no per-token bill). It
never merges — you review in the morning.

Best-of-both design, merging the [Ralph loop](https://ghuntley.com/ralph/) with a
plan-then-interactive-execute flow:

- **Plan** with `claude -p` on the stronger model (**Opus, medium effort**): it
  reads the codebase, judges complexity, and **picks the execution model**
  (`sonnet` for mechanical/localized work, `opus` for genuinely complex). Issues
  labelled `stagedplan` are planned via the **`staged-plan` skill**.
- **Execute** in **one interactive Claude session per issue** on the chosen
  model (medium effort), with **Remote Control** on so you can follow and
  intervene from the Claude mobile app (each session is named `ralph-<n>`). The
  session ends *itself* by printing `RALPH_DONE_EXIT`; a **Stop hook** flags it
  and the runner reclaims the process.
- **GitHub-native**: the `gh issue list --label AFK` queue is processed in
  **ascending issue-number order**. A finished issue (`DONE`) gets a PR
  (`Closes #n`); an explicitly **blocked** issue gets the `HITL` label and is
  not retried; a timeout/incomplete one keeps `AFK` and resumes next run.
- **Subscription-friendly**: no USD cap (there's no API spend). The real limiter
  is your plan's rate limit — on a limit the runner parses the reset time and
  schedules a **resume** via a detached PowerShell process.

```
ralph.ps1
  └─ for each open AFK issue, ascending #, skipping HITL + already-PR'd ones:
       worktree add   (off origin/main, OUTSIDE the repo)
       PLAN     : claude -p (Opus/medium)  → .ralph/plan.md
                  · emits "## Execution model: sonnet|opus" (complexity judgment)
                  · `stagedplan`-labelled issues plan via the staged-plan skill
       EXECUTE  : claude (interactive, chosen model, +Remote Control "ralph-<n>")
                  → does every step, commits each → prints RALPH_DONE_EXIT
                  → Stop hook flags it → runner reclaims the process
       OUTCOME  : DONE    → git push + gh pr create (Closes #n)
                  BLOCKED → comment + label HITL (not retried)
                  timeout → comment, keep AFK (resume next run)
       on rate/usage limit → schedule a resume after reset, stop the run
```

## Files

| File | Role |
|------|------|
| `ralph.ps1` | Orchestrator: queue, worktrees, plan, interactive execute, limit-resume, PR. |
| `prompt.plan.md` | Standard planning pass (`-p`) → `.ralph/plan.md`. |
| `prompt.plan.staged.md` | Planning pass for `stagedplan`-labelled issues — uses the `staged-plan` skill. |
| `prompt.execute.md` | The execution session's charter (copied to `.ralph/exec.md`). |
| `guard.ps1` | `PreToolUse` hook — destructive-command deny-list. |
| `stop_exit_hook.ps1` | `Stop` hook — writes the exit signal to the flag file. |
| `runs/<stamp>/` | Per-run logs + generated `ralph.settings.json`. (gitignored) |
| `../.ralph-worktrees/` | Transient per-issue worktrees, kept **outside the repo** so their paths don't contain `/scripts/ralph/` (which `guard.ps1` blocks). |

The hooks are injected only into the runner's `claude` calls via `--settings`,
so your normal interactive Claude use is untouched.

Execution is **interactive by default** (draws on the subscription quota).
`-HeadlessExec` switches to a `claude -p` loop instead — simpler/headless but
metered at a premium, so use it only where no console/TTY is available.

## How the interactive session self-terminates

This is the crux. For each issue the runner launches `claude` in a **new console
window** (so it gets a TTY) with `--settings` pointing at a Stop hook and an env
var `RALPH_FLAG_FILE`. The initial prompt is passed as a single pre-quoted
argument string (an `-ArgumentList` array drops a multi-word prompt — only the
first word survives):

1. The agent works the plan, then prints `RALPH_DONE_EXIT` (or
   `RALPH_BLOCKED_EXIT <reason>`).
2. On its next turn-end, the **Stop hook** (`stop_exit_hook.ps1`) sees the token
   (from the payload or the transcript) and writes `DONE`/`BLOCKED …` to
   `RALPH_FLAG_FILE`. It does **not** kill anything.
3. The orchestrator polls that file (every 3s). When it appears, it kills the
   process tree (`$proc.Kill($true)`) and moves on. A `-MaxMinutesPerIssue`
   timeout and the global `-DeadlineHours` are the anti-hang backstops.

## Safeguards (unattended)

- **`guard.ps1` deny-list** (`PreToolUse`): blocks `git push`, `reset --hard`,
  `rebase`, branch switches, `gh pr merge/close`, recursive deletes,
  pipe-to-shell, and writes to secrets / `.git/` / the loop's own tooling.
  Required because the run uses `--dangerously-skip-permissions`.
- **Per-issue wall timeout** (`-MaxMinutesPerIssue`, default 45) + global
  **`-DeadlineHours`** (default 8).
- **Worktree isolation**: your main working tree is never touched.
- **PR only on `DONE`**: the agent commits; the orchestrator pushes and opens a
  PR *only* when the agent explicitly finished. Non-DONE outcomes open no PR.
- **Idempotency / queue hygiene**: issues are processed in ascending number
  order; one with an open PR is skipped; one labelled `HITL` (explicitly blocked)
  is skipped until a human clears it; a timeout keeps `AFK` and resumes.
- **Model routing**: planning judges complexity and runs execution on the
  smallest sufficient model (`sonnet` vs `opus`), saving quota.
- **`cargo test` gate**: the execution charter requires green tests before each
  commit.

## Prerequisites

- `claude` (Claude Code CLI), logged in to your **subscription** (no
  `ANTHROPIC_API_KEY`). Auto-located, falls back to
  `%USERPROFILE%\.local\bin\claude.exe`.
- `gh` authenticated (`gh auth status`).
- PowerShell 7 (`pwsh`).
- For `spike`-feature issues, the OpenSSL toolchain (NASM, Strawberry Perl) per
  `CLAUDE.md` — otherwise those builds time out. Prefer leaving `AFK` off
  `spike`-heavy issues for the first runs.

## Usage

```powershell
# 1) Plan only, one issue. No execution, no PR. Inspect .ralph/plan.md.
pwsh -File scripts/ralph/ralph.ps1 -OnlyIssue 13 -DryRun

# 2) One issue, full plan + interactive execution + PR. Follow it from the
#    Claude mobile app (session "ralph-13").
pwsh -File scripts/ralph/ralph.ps1 -OnlyIssue 13

# 3) One issue, full plan + interactive execution, commit locally but NO PR.
pwsh -File scripts/ralph/ralph.ps1 -OnlyIssue 13 -NoPublish

# 4) The overnight run across the whole AFK queue (ascending order).
pwsh -File scripts/ralph/ralph.ps1 -DeadlineHours 8

# Force a model for every issue (overrides the plan's judgment); disable Remote
# Control; or use headless -p (premium-metered) execution:
pwsh -File scripts/ralph/ralph.ps1 -ExecModel opus
pwsh -File scripts/ralph/ralph.ps1 -NoRemoteControl
pwsh -File scripts/ralph/ralph.ps1 -HeadlessExec
```

Morning review: `gh pr list` for the `DONE` issues; issues labelled `HITL` need
you (Claude blocked on them — read the comment); the rest keep `AFK` and will be
retried next run. Reverse a HITL hand-off by removing the label. Discard a dead
branch with `git push origin --delete afk/<n>-…`.

## What's verified

Validated end to end on a live issue (#10):
- **Hooks load from `--settings`** — `PreToolUse` (guard) and `Stop`
  (exit-signal) both fire, so the global `~/.claude/settings.json` is untouched.
- **Plan via `claude -p`** (prompt piped on stdin) produces a real, codebase-
  aware `.ralph/plan.md`.
- **Interactive execution** launches in a new console, implements the whole
  plan, commits, prints `RALPH_DONE_EXIT`; the Stop hook flags it and the runner
  reclaims the process. The produced commit compiled clean (`cargo check`).
- Pure logic (reset-time parser, slug, settings generation) is unit-verified.

Bugs found and fixed during validation (kept here as guardrail rationale):
- `claude -p` ignores a positional prompt when stdout is non-interactive →
  planning pipes the prompt via **stdin**.
- `Start-Process -ArgumentList` (array) drops a multi-word prompt → the
  interactive launch passes a **single pre-quoted argument string**.
- Worktrees under `scripts/ralph/` were blocked wholesale by `guard.ps1` →
  worktrees now live **outside the repo** (`../.ralph-worktrees/`).

Wired but not yet live-validated (sanity-checked in isolation only): the
**`staged-plan` skill** invoked headlessly for `stagedplan` issues (it may
scaffold to `docs/plans/` — the prompt requires `.ralph/plan.md` to be the
authoritative artifact), the **rate-limit resume** path (needs an actual
usage-limit), and **HITL** labelling. The **model router** (plan emits
`## Execution model:`, runner parses it) and **Remote Control** flag are
verified mechanically. Validate new setups incrementally:

1. `-OnlyIssue N -DryRun` — confirms plan generation and `.ralph/plan.md` shape.
2. `-OnlyIssue N -NoPublish` — full interactive execution, commit locally, no
   PR. Inspect the commit (`git -C ../.ralph-worktrees/issue-N log`).
3. `-OnlyIssue N` — same, but pushes a branch and opens a PR.
4. Only then trust the unattended queue.

Known simplifications: the limit scheduler handles intraday resets
(`resets 3:45pm`); a weekday-future reset (`resets Mon 9:30am`) is treated as the
next occurrence of that time, not that weekday. The resume uses a detached
PowerShell process that sleeps until reset — it survives logoff but not a
machine sleep/hibernate; for a laptop overnight, disable sleep or harden this to
a `schtasks` wake-to-run task.
