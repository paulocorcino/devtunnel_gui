#requires -Version 7
<#
.SYNOPSIS
    Ralph runner (Windows): work GitHub issues labelled "AFK" overnight, one PR
    per issue, on your Claude subscription quota (no Anthropic API key).

.DESCRIPTION
    * PLAN with `claude -p` (prompt piped via stdin) -> .ralph/plan.md.
    * EXECUTE by looping `claude -p` (headless, self-terminating). A single -p
      call runs the whole agentic session in one warm context (same token
      economy as an interactive session) and exits on its own; the runner reads
      RALPH_DONE_EXIT / RALPH_BLOCKED_EXIT from its output to classify the
      outcome. The loop is only a safety net for issues too big for one call —
      progress is tracked on disk (plan.md checkboxes + git commits), so a
      second call resumes where the first stopped.
    * GitHub-native: queue from `gh issue list --label AFK`; deliver via a
      branch + `gh pr create` (never merges). Idempotent on open PRs, so an
      incomplete issue is resumed rather than skipped.
    * Subscription-friendly: no USD cap (no API spend). On a rate/usage limit
      the runner parses the reset time and schedules a re-run via a detached
      PowerShell process.

    Hooks (guard deny-list) are injected via --settings, scoped to the runner;
    your global ~/.claude/settings.json is never touched.

.EXAMPLE
    pwsh -File scripts/ralph/ralph.ps1 -OnlyIssue 13 -DryRun       # plan only
.EXAMPLE
    pwsh -File scripts/ralph/ralph.ps1 -OnlyIssue 13 -NoPublish    # exec, no PR
.EXAMPLE
    pwsh -File scripts/ralph/ralph.ps1 -DeadlineHours 8            # overnight
#>
[CmdletBinding()]
param(
    # Wall-clock budget. The runner won't START a new issue past this.
    [double]$DeadlineHours = 8.0,

    # Total time budget for one issue's execution (across -p calls).
    [int]$MaxMinutesPerIssue = 45,

    # Safety-net cap on `claude -p` execution calls per issue.
    [int]$MaxExecCalls = 6,

    # Planning model + effort. Planning runs on the stronger model: it reads the
    # codebase and ALSO judges complexity to pick the execution model.
    [string]$PlanModel = 'opus',
    [string]$PlanEffort = 'medium',

    # Execution model. Empty = chosen per issue from the plan's complexity
    # judgment (sonnet for mechanical/localized work, opus for complex). Set a
    # value to force it for every issue (overrides the judgment).
    [string]$ExecModel = '',
    [string]$ExecEffort = 'medium',

    # Fallback execution model when the plan emits no judgment.
    [string]$DefaultExecModel = 'sonnet',

    # Work only this issue number.
    [int]$OnlyIssue = 0,

    # Plan only; do not execute, push, or open PRs.
    [switch]$DryRun,

    # Execute + commit locally, but do NOT push or open a PR (validation).
    [switch]$NoPublish,

    # Disable scheduling a re-run when a rate/usage limit is hit.
    [switch]$NoResume,

    # Execute via headless `claude -p` loop instead of an interactive session.
    # Default is INTERACTIVE (cheaper on a subscription — headless -p is metered
    # at a premium). Use -HeadlessExec only where no console/TTY is available.
    [switch]$HeadlessExec,

    # Interactive sessions enable Remote Control by default, so you can follow
    # and intervene from the Claude mobile app. -NoRemoteControl disables it.
    [switch]$NoRemoteControl
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# --- Locate tools -------------------------------------------------------------
$ScriptDir = $PSScriptRoot
$RepoRoot  = (git rev-parse --show-toplevel).Trim()
$Claude    = (Get-Command claude -ErrorAction SilentlyContinue)?.Source
if (-not $Claude) { $Claude = "$env:USERPROFILE\.local\bin\claude.exe" }
if (-not (Test-Path $Claude)) { throw "claude CLI not found. Put it on PATH or edit `$Claude." }
$null = (Get-Command gh -ErrorAction Stop)

$RunStamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$RunDir   = Join-Path $ScriptDir "runs\$RunStamp"
# Worktrees live OUTSIDE the repo (a sibling dir): their files must not contain
# "/scripts/ralph/" in their path, or guard.ps1's tooling-protection rule would
# block the agent from editing the issue's own source.
$WtRoot   = Join-Path (Split-Path $RepoRoot -Parent) '.ralph-worktrees'
$StateDir = Join-Path $ScriptDir 'state'        # survives across runs (plan cache)
New-Item -ItemType Directory -Force -Path $RunDir, $WtRoot, $StateDir | Out-Null

$Deadline = (Get-Date).AddHours($DeadlineHours)
$LogFile  = Join-Path $RunDir 'ralph.log'
$script:HaltForResume = $false
$script:LimitText = ''

function Log([string]$msg) {
    $line = "[{0:HH:mm:ss}] {1}" -f (Get-Date), $msg
    $line | Tee-Object -FilePath $LogFile -Append | Write-Host
}

# --- Hooks settings, scoped to the runner -------------------------------------
# PreToolUse guard = destructive-command deny-list (both exec modes).
# Stop hook = records RALPH_DONE_EXIT/BLOCKED to the flag file so the runner can
# reclaim an INTERACTIVE session (interactive Claude never exits on its own).
$GuardCmd = "pwsh -NoProfile -ExecutionPolicy Bypass -File `"$(Join-Path $ScriptDir 'guard.ps1')`""
$StopCmd  = "pwsh -NoProfile -ExecutionPolicy Bypass -File `"$(Join-Path $ScriptDir 'stop_exit_hook.ps1')`""
$Settings = @{
    skipDangerousModePermissionPrompt = $true   # don't hang on the accept prompt
    skipAutoPermissionPrompt          = $true
    autoCompactEnabled                = $false   # don't interrupt a long session
    hooks = @{
        PreToolUse = @(@{ matcher = 'Bash|Edit|Write|MultiEdit|NotebookEdit'
                          hooks   = @(@{ type = 'command'; command = $GuardCmd }) })
        Stop       = @(@{ matcher = ''
                          hooks   = @(@{ type = 'command'; command = $StopCmd }) })
    }
}
$SettingsPath = Join-Path $RunDir 'ralph.settings.json'
$Settings | ConvertTo-Json -Depth 8 | Set-Content -Path $SettingsPath -Encoding utf8

# Guarantee subscription billing: clear any inherited API key for this process tree.
$env:ANTHROPIC_API_KEY = ''

# --- Helpers ------------------------------------------------------------------
function New-Slug([string]$title) {
    $s = $title.ToLowerInvariant() -replace '[^a-z0-9]+', '-' -replace '(^-+|-+$)', ''
    if ($s.Length -gt 40) { $s = $s.Substring(0, 40).TrimEnd('-') }
    return $s
}

function Get-OpenSteps([string]$PlanPath) {
    if (-not (Test-Path $PlanPath)) { return -1 }
    return (Select-String -Path $PlanPath -Pattern '^\s*-\s*\[ \]' -AllMatches).Count
}

# Read the planner's complexity judgment: "## Execution model: sonnet|opus".
function Get-RecommendedModel([string]$PlanPath) {
    if (-not (Test-Path $PlanPath)) { return '' }
    $m = Select-String -Path $PlanPath -Pattern '^\s*##\s*Execution model:\s*(opus|sonnet)' | Select-Object -First 1
    if ($m) { return $m.Matches[0].Groups[1].Value.ToLower() }
    return ''
}

function Test-LimitText([string]$text) {
    return [bool]($text -match '(?i)(rate limit|usage limit|reached your .* limit|limit reached|resets\s+\d)')
}

# PLAN: one-shot `claude -p`, prompt piped via STDIN (a positional prompt is
# ignored when stdout is non-interactive). Writes .ralph/plan.md inside $Cwd.
function Invoke-Plan {
    param([string]$Cwd, [string]$PromptText, [string]$OutLog, [switch]$Staged)
    $a = @('-p', '--dangerously-skip-permissions', '--settings', $SettingsPath)
    if ($PlanEffort) { $a += @('--effort', $PlanEffort) }
    if ($PlanModel)  { $a = @('--model', $PlanModel) + $a }
    if ($Staged) { $env:STAGED_PLAN_NONINTERACTIVE = '1' }  # staged-plan skill: no AskUserQuestion
    Push-Location $Cwd
    try { ($PromptText | & $Claude @a 2>&1) | Set-Content -Path $OutLog -Encoding utf8 }
    finally {
        Pop-Location
        if ($Staged) { Remove-Item Env:\STAGED_PLAN_NONINTERACTIVE -ErrorAction SilentlyContinue }
    }
}

# One execution call: `claude -p` with the prompt on stdin, captured output,
# and a hard timeout. Returns $true if it exited within the timeout.
function Invoke-ExecCall {
    param([string]$Cwd, [string]$PromptFile, [string]$OutFile, [string]$ErrFile, [int]$TimeoutMs, [string]$Model)
    $a = @('-p', '--dangerously-skip-permissions', '--settings', $SettingsPath)
    if ($Model)      { $a += @('--model', $Model) }
    if ($ExecEffort) { $a += @('--effort', $ExecEffort) }
    $p = Start-Process $Claude -ArgumentList $a -WorkingDirectory $Cwd -NoNewWindow -PassThru `
            -RedirectStandardInput $PromptFile -RedirectStandardOutput $OutFile -RedirectStandardError $ErrFile
    if (-not $p.WaitForExit($TimeoutMs)) { try { $p.Kill($true) } catch {}; return $false }
    return $true
}

# EXECUTE loop: run -p calls until DONE / BLOCKED / stuck / timeout / cap.
function Invoke-ExecLoop {
    param([string]$Cwd, [string]$PlanPath, [string]$IssueRun, [string]$PromptFile, [string]$Model)
    $issueDeadline = (Get-Date).AddMinutes($MaxMinutesPerIssue)
    $stuck = 0
    for ($i = 1; $i -le $MaxExecCalls; $i++) {
        $remMs = [int][math]::Min(($issueDeadline - (Get-Date)).TotalMilliseconds, ($Deadline - (Get-Date)).TotalMilliseconds)
        if ($remMs -le 5000) { return 'timeout' }

        $before = (git -C $Cwd rev-parse HEAD).Trim()
        $of = Join-Path $IssueRun "exec-$i.out"; $ef = Join-Path $IssueRun "exec-$i.err"
        $exited = Invoke-ExecCall -Cwd $Cwd -PromptFile $PromptFile -OutFile $of -ErrFile $ef -TimeoutMs $remMs -Model $Model
        $after  = (git -C $Cwd rev-parse HEAD).Trim()

        $out = ((Get-Content $of -Raw -ErrorAction SilentlyContinue) + "`n" + (Get-Content $ef -Raw -ErrorAction SilentlyContinue))
        $open = Get-OpenSteps $PlanPath
        $did  = $before -ne $after
        Log "    exec call ${i}: exited=$exited open=$open committed=$did"

        if (Test-LimitText $out) { return 'limit' }
        if (-not $exited)        { return 'timeout' }

        $m = [regex]::Match($out, 'RALPH_BLOCKED_EXIT\s*(.*)')
        if ($m.Success) { return "BLOCKED $($m.Groups[1].Value.Trim())" }
        if ($open -eq 0 -or $out -match 'RALPH_DONE_EXIT') { return 'DONE' }

        if ($did) { $stuck = 0 } else { $stuck++ }
        if ($stuck -ge 2) { return 'stuck' }
    }
    return 'maxcalls'
}

# INTERACTIVE execution: launch a real Claude session in a new console window
# (so it gets a TTY), poll the flag file the Stop hook writes, then reclaim it.
# This is the default — interactive draws on the subscription quota, whereas
# headless `-p` is metered at a premium.
function Invoke-Interactive {
    param([string]$Cwd, [string]$InitialPrompt, [string]$FlagFile, [string]$Model, [string]$Name)
    Remove-Item -LiteralPath $FlagFile -ErrorAction SilentlyContinue
    $env:RALPH_FLAG_FILE = $FlagFile           # inherited by claude -> the Stop hook

    # Build a SINGLE pre-quoted command line. Passing -ArgumentList as an array
    # makes Start-Process drop/split a multi-word positional prompt (only the
    # first word survives); a single string with the prompt double-quoted is
    # delivered intact.
    $promptArg = $InitialPrompt -replace '"', '\"'
    $argString = "--dangerously-skip-permissions --settings `"$SettingsPath`""
    if (-not $NoRemoteControl) { $argString += " --remote-control `"$Name`"" }  # follow from mobile
    if ($Model)      { $argString += " --model $Model" }
    if ($ExecEffort) { $argString += " --effort $ExecEffort" }
    $argString += " `"$promptArg`""

    # A console app launched without -NoNewWindow gets its own console window/TTY.
    $proc = Start-Process -FilePath $Claude -ArgumentList $argString -WorkingDirectory $Cwd -PassThru
    $issueDeadline = (Get-Date).AddMinutes($MaxMinutesPerIssue)

    $status = 'unknown'
    while ($true) {
        Start-Sleep -Seconds 3
        if (Test-Path $FlagFile)           { $status = (Get-Content $FlagFile -Raw).Trim(); try { $proc.Kill($true) } catch {}; break }
        if ($proc.HasExited)               { $status = if (Test-Path $FlagFile) { (Get-Content $FlagFile -Raw).Trim() } else { 'exited' }; break }
        if ((Get-Date) -ge $issueDeadline) { $status = 'timeout';  try { $proc.Kill($true) } catch {}; break }
        if ((Get-Date) -ge $Deadline)      { $status = 'deadline'; try { $proc.Kill($true) } catch {}; break }
    }
    Remove-Item Env:\RALPH_FLAG_FILE -ErrorAction SilentlyContinue
    return $status
}

function Get-LatestTranscript {
    $base = Join-Path $env:USERPROFILE '.claude\projects'
    if (-not (Test-Path $base)) { return $null }
    $f = Get-ChildItem $base -Recurse -Filter *.jsonl -ErrorAction SilentlyContinue |
         Sort-Object LastWriteTime -Descending | Select-Object -First 1
    if ($f -and ((Get-Date) - $f.LastWriteTime).TotalSeconds -lt 300) { return $f }
    return $null
}

# --- Rate-limit reset parsing + re-run scheduling -----------------------------
function Get-ResetDateTime([string]$text) {
    $m = [regex]::Match($text, 'resets\s+(?:([A-Za-z]{3})\s+)?(\d{1,2}:\d{2}\s*[ap]m)', 'IgnoreCase')
    if (-not $m.Success) { return $null }
    $timeStr = $m.Groups[2].Value -replace '\s', ''
    $t = [regex]::Match($timeStr, '(\d{1,2}):(\d{2})([ap]m)', 'IgnoreCase')
    if (-not $t.Success) { return $null }
    $hour = [int]$t.Groups[1].Value; $min = [int]$t.Groups[2].Value; $ap = $t.Groups[3].Value.ToLower()
    if ($ap -eq 'pm' -and $hour -ne 12) { $hour += 12 } elseif ($ap -eq 'am' -and $hour -eq 12) { $hour = 0 }
    $reset = (Get-Date).Date.AddHours($hour).AddMinutes($min)
    if ($reset -le (Get-Date)) { $reset = $reset.AddDays(1) }
    return $reset
}

function Schedule-Rerun([datetime]$ResetDt) {
    if ($NoResume) { return }
    $retry = $ResetDt.AddMinutes(5)
    $wait  = [int][math]::Max(60, ($retry - (Get-Date)).TotalSeconds)
    $tail  = if ($NoPublish) { ' -NoPublish' } else { '' }
    $cmd   = "Start-Sleep -Seconds $wait; & pwsh -NoProfile -File `"$PSCommandPath`" -DeadlineHours $DeadlineHours -ExecModel $ExecModel -ExecEffort $ExecEffort$tail"
    Start-Process pwsh -ArgumentList '-NoProfile', '-WindowStyle', 'Hidden', '-Command', $cmd | Out-Null
    Log "  LIMIT: resets $($ResetDt.ToString('HH:mm')) -> re-run scheduled at $($retry.ToString('HH:mm')) (sleeps ${wait}s)."
}

# --- Publish ------------------------------------------------------------------
function Publish-Result {
    param([int]$IssueNum, [string]$Title, [string]$Branch, [string]$Wt, [string]$Status)
    $commits = (git -C $Wt rev-list --count "origin/main..HEAD").Trim()
    Log "  result: status=$Status commits=$commits"

    if ($NoPublish) {
        Log "  [NoPublish] not pushing. Branch '$Branch' kept at $Wt with $commits commit(s)."
        if ([int]$commits -gt 0) { (git -C $Wt log --oneline "origin/main..HEAD") | ForEach-Object { Log "    $_" } }
        return
    }

    # Open a PR ONLY when the agent explicitly finished (DONE).
    if ($Status -ne 'DONE') {
        if ($Status -like 'BLOCKED*') {
            # Explicit block: Claude decided it can't proceed autonomously. Hand
            # it to a human with the HITL label (created on demand) — the queue
            # skips HITL issues, so it is not retried until a human clears it.
            $reason = ($Status -replace '^BLOCKED', '').Trim()
            gh label create HITL --color B60205 --description "Needs a human (Ralph blocked)" 2>$null | Out-Null
            gh issue edit $IssueNum --add-label HITL 2>&1 | Out-Null
            gh issue comment $IssueNum --body "Ralph: blocked — $reason. Labelled **HITL**; will not retry until a human resolves it." | Out-Null
            Log "  blocked -> labelled #$IssueNum HITL"
        } else {
            # Timeout / incomplete session: transient. Keep AFK and resume next run.
            $note = if ([int]$commits -gt 0) { " $commits local commit(s) kept on '$Branch' for resume." } else { '' }
            gh issue comment $IssueNum --body "Ralph: did not finish ($Status). No PR opened.$note Will resume on the next run." | Out-Null
        }
        return
    }
    if ([int]$commits -le 0) {
        gh issue comment $IssueNum --body "Ralph: reported DONE but produced no commits — nothing to open a PR with." | Out-Null
        return
    }

    git -C $Wt push -u origin $Branch --quiet
    $body = @"
Autonomous Ralph PR for issue #$IssueNum ($commits commit(s)).

Closes #$IssueNum

> Generated overnight on the subscription quota. Review before merging.
"@
    $pr = gh pr create --base main --head $Branch --title "$Title (#$IssueNum)" --body $body 2>&1
    Log "  PR: $pr"
    gh issue comment $IssueNum --body "Ralph: DONE — opened $pr ($commits commits)." | Out-Null
}

# --- Run one issue end to end -------------------------------------------------
function Invoke-Issue {
    param([int]$IssueNum, [string]$Title, [switch]$StagedPlan)

    $issueRun = Join-Path $RunDir "issue-$IssueNum"
    New-Item -ItemType Directory -Force -Path $issueRun | Out-Null
    Log "=== #$IssueNum  $Title"

    # Reuse an existing afk/<n>-* branch (resume an incomplete issue) or start fresh.
    $remoteBranch = (git ls-remote --heads origin "afk/$IssueNum-*" 2>$null | ForEach-Object { ($_ -split '\s+')[1] -replace '^refs/heads/', '' } | Select-Object -First 1)
    $localBranch  = (git branch --list "afk/$IssueNum-*" | ForEach-Object { $_.TrimStart('* ').Trim() } | Select-Object -First 1)
    $branch = $remoteBranch ?? $localBranch ?? "afk/$IssueNum-$(New-Slug $Title)"
    $wt = Join-Path $WtRoot "issue-$IssueNum"
    Log "  branch=$branch"

    if (Test-Path $wt) { git worktree remove --force $wt 2>$null }
    if ($remoteBranch) {
        git worktree add $wt $branch --quiet
    } elseif ($localBranch) {
        git worktree add $wt $branch --quiet
    } else {
        git worktree add -b $branch $wt origin/main --quiet
    }
    if ($LASTEXITCODE -ne 0) { Log "  ! could not create worktree — skipping."; return }

    try {
        $ralphDir = Join-Path $wt '.ralph'
        New-Item -ItemType Directory -Force -Path $ralphDir | Out-Null
        gh issue view $IssueNum --json number,title,body,labels | Set-Content (Join-Path $ralphDir 'issue.json') -Encoding utf8
        Copy-Item (Join-Path $ScriptDir 'prompt.execute.md') (Join-Path $ralphDir 'exec.md') -Force

        $planPath  = Join-Path $ralphDir 'plan.md'
        $planCache = Join-Path $StateDir "issue-$IssueNum\plan.md"  # survives across runs (.ralph is gitignored)

        if (Test-Path $planCache) {
            Copy-Item $planCache $planPath -Force
            Log "  resuming with cached plan ($(Get-OpenSteps $planPath) open step(s))."
        } else {
            $planPrompt = if ($StagedPlan) { 'prompt.plan.staged.md' } else { 'prompt.plan.md' }
            Log "  planning… [$(if($StagedPlan){'staged-plan skill'}else{'standard'})]"
            Invoke-Plan -Cwd $wt -PromptText (Get-Content (Join-Path $ScriptDir $planPrompt) -Raw) -OutLog (Join-Path $issueRun 'plan.log') -Staged:$StagedPlan
            $open = Get-OpenSteps $planPath
            if ($open -lt 0) { Log "  no plan written — skipping."; if (-not $NoPublish) { gh issue comment $IssueNum --body "Ralph: planning produced no plan file. Skipped." | Out-Null }; return }
            if ($open -eq 0) { Log "  no actionable steps — infeasible."; if (-not $NoPublish) { gh issue comment $IssueNum --body "Ralph: planning found no actionable, autonomously-verifiable steps. Skipped." | Out-Null }; return }
            New-Item -ItemType Directory -Force -Path (Split-Path $planCache) | Out-Null
            Copy-Item $planPath $planCache -Force
            Copy-Item $planPath (Join-Path $issueRun 'plan.md') -Force
            Log "  plan: $open open step(s)"
        }

        if ($DryRun) { Log "  [DryRun] plan saved to $(Join-Path $issueRun 'plan.md') (worktree kept at $wt)."; return }

        # --- Choose execution model: explicit override > plan judgment > default.
        if ($ExecModel) {
            $execModel = $ExecModel; $why = 'forced'
        } else {
            $execModel = Get-RecommendedModel $planPath
            if ($execModel) { $why = 'plan judgment' } else { $execModel = $DefaultExecModel; $why = 'default (no judgment)' }
        }
        Log "  exec model: $execModel/$ExecEffort [$why]"

        # --- Execution ---
        $script:LimitText = ''
        if ($HeadlessExec) {
            $promptFile = Join-Path $issueRun 'exec-prompt.in'
            Get-Content (Join-Path $ScriptDir 'prompt.execute.md') -Raw | Set-Content $promptFile -Encoding utf8
            Log "  executing [headless -p]…"
            $status = Invoke-ExecLoop -Cwd $wt -PlanPath $planPath -IssueRun $issueRun -PromptFile $promptFile -Model $execModel
        } else {
            Log "  executing [interactive$(if(-not $NoRemoteControl){' +remote'})]…"
            $flag = Join-Path $issueRun 'status.flag'
            $status = Invoke-Interactive -Cwd $wt -FlagFile $flag -Model $execModel -Name "ralph-$IssueNum" `
                -InitialPrompt 'Read .ralph/exec.md and follow it exactly to implement .ralph/plan.md for this issue. Emit RALPH_DONE_EXIT when finished.'
            # Interactive sessions exit on a usage limit; detect it from the transcript.
            if ($status -in @('exited', 'timeout', 'deadline', 'unknown')) {
                $tr = Get-LatestTranscript
                if ($tr) { $txt = Get-Content $tr.FullName -Raw; if (Test-LimitText $txt) { $status = 'limit'; $script:LimitText = $txt } }
            }
        }
        Log "  execution ended: $status"

        # Refresh the cached plan with the agent's checkbox progress.
        if (Test-Path $planPath) { Copy-Item $planPath $planCache -Force }

        if ($status -eq 'limit') {
            $reset = if ($script:LimitText) { Get-ResetDateTime $script:LimitText } else { $null }
            if (-not $reset) {
                $lastErr = Get-ChildItem $issueRun -Filter 'exec-*.err' -ErrorAction SilentlyContinue | Sort-Object Name | Select-Object -Last 1
                if ($lastErr) { $reset = Get-ResetDateTime (Get-Content $lastErr.FullName -Raw) }
            }
            if (-not $reset) { $reset = (Get-Date).AddMinutes(60) }   # fallback: try again in an hour
            Schedule-Rerun $reset
            $script:HaltForResume = $true
            return
        }

        Publish-Result -IssueNum $IssueNum -Title $Title -Branch $branch -Wt $wt -Status $status
    }
    catch { Log "  ! error on #${IssueNum}: $($_.Exception.Message)" }
    finally {
        # Keep the worktree when inspecting (DryRun/NoPublish) or resuming.
        if (-not $script:HaltForResume -and -not $DryRun -and -not $NoPublish) { git worktree remove --force $wt 2>$null }
    }
}

# --- Main ---------------------------------------------------------------------
Log "Ralph run $RunStamp | deadline=$($Deadline.ToString('HH:mm')) perIssue=${MaxMinutesPerIssue}min plan=$PlanModel/$PlanEffort exec=$(if($ExecModel){$ExecModel}else{'auto'})/$ExecEffort$(if($HeadlessExec){' [headless]'}else{' [interactive]'})$(if($NoPublish){' [NoPublish]'})$(if($DryRun){' [DryRun]'})"
git fetch origin --quiet

$issues = (gh issue list --label AFK --state open --json number,title,labels --limit 100) | ConvertFrom-Json
if ($OnlyIssue -gt 0) { $issues = $issues | Where-Object { $_.number -eq $OnlyIssue } }
# Skip issues a previous run handed off to a human (labelled HITL).
$issues = @($issues | Where-Object { $_.labels.name -notcontains 'HITL' })
if (-not $issues) { Log "No open AFK issues to process. Done."; return }
# Respect task sequence: process in ascending issue-number order (#5, #6, #9 ...).
$issues = @($issues | Sort-Object number)
Log "Queue: $($issues.Count) issue(s) in order: $((($issues | ForEach-Object { '#' + $_.number }) -join ' -> '))"

# Skip issues that already have an OPEN PR (delivered/in-review); incomplete
# branches without a PR are resumed.
$openPrHeads = @()
if (-not $NoPublish) {
    $prs = gh pr list --state open --json headRefName | ConvertFrom-Json
    if ($prs) { $openPrHeads = @($prs.headRefName) }   # empty when there are no open PRs
}

foreach ($issue in $issues) {
    if ((Get-Date) -ge $Deadline) { Log "DEADLINE reached. Stopping."; break }
    if ($openPrHeads | Where-Object { $_ -like "afk/$($issue.number)-*" }) { Log "#$($issue.number) already has an open PR — skipping."; continue }

    $staged = $issue.labels.name -contains 'stagedplan'
    Invoke-Issue -IssueNum $issue.number -Title $issue.title -StagedPlan:$staged
    if ($script:HaltForResume) { Log "Stopping run; re-run is scheduled."; break }
}

Log "Ralph run complete. Logs: $RunDir"
