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

    # Model alias for the planning `-p` call ('' = configured default).
    [string]$PlanModel = 'opus',

    # Model alias for the execution `-p` calls (cheaper = stretches quota).
    [string]$ExecModel = 'sonnet',

    # Reasoning effort for execution ('' to omit the flag).
    [string]$ExecEffort = 'low',

    # Work only this issue number.
    [int]$OnlyIssue = 0,

    # Plan only; do not execute, push, or open PRs.
    [switch]$DryRun,

    # Execute + commit locally, but do NOT push or open a PR (validation).
    [switch]$NoPublish,

    # Disable scheduling a re-run when a rate/usage limit is hit.
    [switch]$NoResume
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
$WtRoot   = Join-Path $ScriptDir 'worktrees'
$StateDir = Join-Path $ScriptDir 'state'        # survives across runs (plan cache)
New-Item -ItemType Directory -Force -Path $RunDir, $WtRoot, $StateDir | Out-Null

$Deadline = (Get-Date).AddHours($DeadlineHours)
$LogFile  = Join-Path $RunDir 'ralph.log'
$script:HaltForResume = $false

function Log([string]$msg) {
    $line = "[{0:HH:mm:ss}] {1}" -f (Get-Date), $msg
    $line | Tee-Object -FilePath $LogFile -Append | Write-Host
}

# --- Hooks settings (guard deny-list), scoped to the runner -------------------
$GuardCmd = "pwsh -NoProfile -ExecutionPolicy Bypass -File `"$(Join-Path $ScriptDir 'guard.ps1')`""
$Settings = @{
    skipDangerousModePermissionPrompt = $true   # don't hang on the accept prompt
    skipAutoPermissionPrompt          = $true
    autoCompactEnabled                = $false   # don't interrupt a long -p session
    hooks = @{
        PreToolUse = @(@{ matcher = 'Bash|Edit|Write|MultiEdit|NotebookEdit'
                          hooks   = @(@{ type = 'command'; command = $GuardCmd }) })
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

function Test-LimitText([string]$text) {
    return [bool]($text -match '(?i)(rate limit|usage limit|reached your .* limit|limit reached|resets\s+\d)')
}

# PLAN: one-shot `claude -p`, prompt piped via STDIN (a positional prompt is
# ignored when stdout is non-interactive). Writes .ralph/plan.md inside $Cwd.
function Invoke-Plan {
    param([string]$Cwd, [string]$PromptText, [string]$OutLog)
    $a = @('-p', '--dangerously-skip-permissions', '--settings', $SettingsPath)
    if ($PlanModel) { $a = @('--model', $PlanModel) + $a }
    Push-Location $Cwd
    try { ($PromptText | & $Claude @a 2>&1) | Set-Content -Path $OutLog -Encoding utf8 }
    finally { Pop-Location }
}

# One execution call: `claude -p` with the prompt on stdin, captured output,
# and a hard timeout. Returns $true if it exited within the timeout.
function Invoke-ExecCall {
    param([string]$Cwd, [string]$PromptFile, [string]$OutFile, [string]$ErrFile, [int]$TimeoutMs)
    $a = @('-p', '--dangerously-skip-permissions', '--settings', $SettingsPath)
    if ($ExecModel)  { $a += @('--model', $ExecModel) }
    if ($ExecEffort) { $a += @('--effort', $ExecEffort) }
    $p = Start-Process $Claude -ArgumentList $a -WorkingDirectory $Cwd -NoNewWindow -PassThru `
            -RedirectStandardInput $PromptFile -RedirectStandardOutput $OutFile -RedirectStandardError $ErrFile
    if (-not $p.WaitForExit($TimeoutMs)) { try { $p.Kill($true) } catch {}; return $false }
    return $true
}

# EXECUTE loop: run -p calls until DONE / BLOCKED / stuck / timeout / cap.
function Invoke-ExecLoop {
    param([string]$Cwd, [string]$PlanPath, [string]$IssueRun, [string]$PromptFile)
    $issueDeadline = (Get-Date).AddMinutes($MaxMinutesPerIssue)
    $stuck = 0
    for ($i = 1; $i -le $MaxExecCalls; $i++) {
        $remMs = [int][math]::Min(($issueDeadline - (Get-Date)).TotalMilliseconds, ($Deadline - (Get-Date)).TotalMilliseconds)
        if ($remMs -le 5000) { return 'timeout' }

        $before = (git -C $Cwd rev-parse HEAD).Trim()
        $of = Join-Path $IssueRun "exec-$i.out"; $ef = Join-Path $IssueRun "exec-$i.err"
        $exited = Invoke-ExecCall -Cwd $Cwd -PromptFile $PromptFile -OutFile $of -ErrFile $ef -TimeoutMs $remMs
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
    if ([int]$commits -le 0) {
        $why = if ($Status -like 'BLOCKED*') { $Status -replace '^BLOCKED', 'blocked:' } else { "no progress ($Status)" }
        gh issue comment $IssueNum --body "Ralph: $why. No commits, no PR opened." | Out-Null
        return
    }
    git -C $Wt push -u origin $Branch --quiet
    $done = $Status -eq 'DONE'
    $tag  = if ($done) { "Closes #$IssueNum" } else { "Refs #$IssueNum (partial: $Status)" }
    $ttl  = if ($done) { "$Title (#$IssueNum)" } else { "[WIP] $Title (#$IssueNum)" }
    $body = @"
Autonomous Ralph PR for issue #$IssueNum.

Status: **$Status** ($commits commit(s)).
$tag

> Generated overnight on the subscription quota. Review before merging.
"@
    $pr = gh pr create --base main --head $Branch --title $ttl --body $body 2>&1
    Log "  PR: $pr"
    gh issue comment $IssueNum --body "Ralph: opened $pr — status **$Status** ($commits commits)." | Out-Null
}

# --- Run one issue end to end -------------------------------------------------
function Invoke-Issue {
    param([int]$IssueNum, [string]$Title)

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
            Log "  planning…"
            Invoke-Plan -Cwd $wt -PromptText (Get-Content (Join-Path $ScriptDir 'prompt.plan.md') -Raw) -OutLog (Join-Path $issueRun 'plan.log')
            $open = Get-OpenSteps $planPath
            if ($open -lt 0) { Log "  no plan written — skipping."; if (-not $NoPublish) { gh issue comment $IssueNum --body "Ralph: planning produced no plan file. Skipped." | Out-Null }; return }
            if ($open -eq 0) { Log "  no actionable steps — infeasible."; if (-not $NoPublish) { gh issue comment $IssueNum --body "Ralph: planning found no actionable, autonomously-verifiable steps. Skipped." | Out-Null }; return }
            New-Item -ItemType Directory -Force -Path (Split-Path $planCache) | Out-Null
            Copy-Item $planPath $planCache -Force
            Copy-Item $planPath (Join-Path $issueRun 'plan.md') -Force
            Log "  plan: $open open step(s)"
        }

        if ($DryRun) { Log "  [DryRun] plan saved to $(Join-Path $issueRun 'plan.md') (worktree kept at $wt)."; return }

        # --- Execution ---
        $promptFile = Join-Path $issueRun 'exec-prompt.in'
        Get-Content (Join-Path $ScriptDir 'prompt.execute.md') -Raw | Set-Content $promptFile -Encoding utf8
        Log "  executing (model=$ExecModel effort=$ExecEffort)…"
        $status = Invoke-ExecLoop -Cwd $wt -PlanPath $planPath -IssueRun $issueRun -PromptFile $promptFile
        Log "  execution ended: $status"

        # Refresh the cached plan with the agent's checkbox progress.
        if (Test-Path $planPath) { Copy-Item $planPath $planCache -Force }

        if ($status -eq 'limit') {
            $lastErr = Get-ChildItem $issueRun -Filter 'exec-*.err' | Sort-Object Name | Select-Object -Last 1
            $reset = if ($lastErr) { Get-ResetDateTime (Get-Content $lastErr.FullName -Raw) } else { $null }
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
Log "Ralph run $RunStamp | deadline=$($Deadline.ToString('HH:mm')) perIssue=${MaxMinutesPerIssue}min exec=$ExecModel/$ExecEffort$(if($NoPublish){' [NoPublish]'})$(if($DryRun){' [DryRun]'})"
git fetch origin --quiet

$issues = (gh issue list --label AFK --state open --json number,title --limit 100) | ConvertFrom-Json
if ($OnlyIssue -gt 0) { $issues = $issues | Where-Object { $_.number -eq $OnlyIssue } }
if (-not $issues) { Log "No open AFK issues. Done."; return }
Log "Queue: $($issues.Count) issue(s): $((($issues | ForEach-Object { '#' + $_.number }) -join ', '))"

# Skip issues that already have an OPEN PR (delivered/in-review); incomplete
# branches without a PR are resumed.
$openPrHeads = @()
if (-not $NoPublish) { $openPrHeads = (gh pr list --state open --json headRefName | ConvertFrom-Json).headRefName }

foreach ($issue in $issues) {
    if ((Get-Date) -ge $Deadline) { Log "DEADLINE reached. Stopping."; break }
    if ($openPrHeads | Where-Object { $_ -like "afk/$($issue.number)-*" }) { Log "#$($issue.number) already has an open PR — skipping."; continue }

    Invoke-Issue -IssueNum $issue.number -Title $issue.title
    if ($script:HaltForResume) { Log "Stopping run; re-run is scheduled."; break }
}

Log "Ralph run complete. Logs: $RunDir"
