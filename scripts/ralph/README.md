# Ralph runner — autonomous overnight issue worker (Windows)

Works GitHub issues labelled **`AFK`** unattended, **one PR per issue**, on your
Claude **subscription quota** (no Anthropic API key, so no per-token bill). It
never merges — you review in the morning.

Best-of-both design, merging the [Ralph loop](https://ghuntley.com/ralph/) with a
plan-then-interactive-execute flow:

- **Plan** with `claude -p` (cheap, non-interactive) → writes `.ralph/plan.md`.
- **Execute** in **one interactive Claude session per issue**. A single warm
  session does every plan step (far fewer tokens than re-priming context per
  step). The session ends *itself* by printing `RALPH_DONE_EXIT`; a **Stop
  hook** records that to a flag file and the runner reclaims the process.
- **GitHub-native**: queue from `gh issue list --label AFK`; deliver via a
  branch + `gh pr create`.
- **Subscription-friendly scheduling**: no USD cap (there's no API spend). The
  real limiter is your plan's rate limit — on a limit the runner parses the
  reset time from the transcript and schedules a **resume** via a detached
  PowerShell process.

```
ralph.ps1
  └─ for each open AFK issue (skip if a branch already exists):
       git worktree add -b afk/<n>-<slug> off origin/main      (isolation)
       PLAN     : claude -p  prompt.plan.md   → .ralph/plan.md  (checklist)
       EXECUTE  : claude (interactive)  → does every step, commits each
                  → prints RALPH_DONE_EXIT → Stop hook flags it → runner kills it
       PUBLISH  : git push + gh pr create   ([WIP] if partial / blocked)
       COMMENT  : gh issue comment   (outcome)
       on rate/usage limit → schedule a resume after reset, stop the run
```

## Files

| File | Role |
|------|------|
| `ralph.ps1` | Orchestrator: queue, worktrees, plan, interactive execute, limit-resume, PR. |
| `prompt.plan.md` | Planning pass (`-p`) → `.ralph/plan.md`. |
| `prompt.execute.md` | The interactive session's charter (copied to `.ralph/exec.md`). |
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
- **PR-only**: the agent commits; the orchestrator pushes and opens the PR.
- **Idempotency**: an issue whose `afk/<n>-*` branch already exists is skipped.
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

# 2) One issue, full plan + interactive execution + PR. Watch it.
pwsh -File scripts/ralph/ralph.ps1 -OnlyIssue 13

# 3) One issue, full plan + interactive execution, commit locally but NO PR.
pwsh -File scripts/ralph/ralph.ps1 -OnlyIssue 13 -NoPublish

# 4) The overnight run across the whole AFK queue.
pwsh -File scripts/ralph/ralph.ps1 -DeadlineHours 8

# Stronger/cheaper execution model, or headless -p (premium-metered) execution:
pwsh -File scripts/ralph/ralph.ps1 -ExecModel sonnet -ExecEffort low
pwsh -File scripts/ralph/ralph.ps1 -HeadlessExec
```

Morning review: `gh pr list`; each issue has a comment with the outcome
(`DONE` / `BLOCKED …` / `timeout` / `deadline`). Partial work lands as `[WIP]`
PRs. Discard a dead branch with `git push origin --delete afk/<n>-…`.

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

Still to validate with a live session: the **rate-limit resume** path (needs an
actual usage-limit to trigger). Validate new setups incrementally:

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
