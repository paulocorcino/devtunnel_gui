#requires -Version 7
<#
.SYNOPSIS
    Ralph runner (Windows): work GitHub issues labelled "AFK" overnight onto a
    SINGLE run branch, on your Claude subscription quota (no Anthropic API key).

.DESCRIPTION
    * One branch per RUN: the runner creates `afk/run-<stamp>` from a base you
      choose (-BaseBranch, default origin/main) in a throwaway worktree, then
      works the AFK queue in order, committing every issue onto that same
      branch. At the end you have ONE branch to review and merge back into the
      base by hand. The runner never pushes and never opens a PR.
    * PLAN with `claude -p` (prompt piped via stdin) -> .ralph/plan.md.
    * EXECUTE by looping `claude -p` (headless, self-terminating) or an
      interactive session. The runner reads RALPH_DONE_EXIT / RALPH_BLOCKED_EXIT
      from the output to classify each issue's outcome.
    * Stop-at-first-block: the moment an issue does NOT finish green (block,
      timeout, stuck, usage limit), the run stops and hands you the branch as
      it stands. Completed issues stay committed; the stalled issue's partial
      commits are left in place for you to inspect.
    * Subscription-friendly: no USD cap (no API spend). A usage limit is treated
      as a stop — the runner reports the reset time and you re-run manually.

    Hooks (guard deny-list) are injected via --settings, scoped to the runner;
    your global ~/.claude/settings.json is never touched.

.EXAMPLE
    pwsh -File scripts/ralph/ralph.ps1 -OnlyIssue 13 -DryRun       # plan only
.EXAMPLE
    pwsh -File scripts/ralph/ralph.ps1 -DeadlineHours 8            # overnight
.EXAMPLE
    pwsh -File scripts/ralph/ralph.ps1 -BaseBranch feature/x       # cut from a branch
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

    # Base the run branch is cut from (and that you merge it back into by hand).
    # Any commit-ish: a remote-tracking branch (origin/main), a local branch, a
    # tag, or a SHA. Defaults to origin/main.
    [string]$BaseBranch = 'origin/main',

    # Plan only; do not execute. The run branch is still created so you can
    # inspect the plans, but no source changes are made.
    [switch]$DryRun,

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
# The run worktree lives OUTSIDE the repo (a sibling dir): its files must not
# contain "/scripts/ralph/" in their path, or guard.ps1's tooling-protection
# rule would block the agent from editing an issue's own source.
$WtRoot   = Join-Path (Split-Path $RepoRoot -Parent) '.ralph-worktrees'
New-Item -ItemType Directory -Force -Path $RunDir, $WtRoot | Out-Null

$Deadline = (Get-Date).AddHours($DeadlineHours)
$LogFile  = Join-Path $RunDir 'ralph.log'
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
function Get-OpenSteps([string]$PlanPath) {
    if (-not (Test-Path $PlanPath)) { return -1 }
    # @() so an empty match set yields 0, not a StrictMode "Count not found" crash.
    return @(Select-String -Path $PlanPath -Pattern '^\s*-\s*\[ \]' -AllMatches).Count
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

# Best-effort parse of a usage-limit reset time, for the stop report only.
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

# EXECUTE loop (headless): run -p calls until DONE / BLOCKED / stuck / timeout / cap.
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

        if (Test-LimitText $out) { $script:LimitText = $out; return 'limit' }
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

# --- Run one issue onto the shared run branch ---------------------------------
# Plans, then executes, committing onto $Wt's current branch. Returns the
# outcome string. 'DONE' means the issue finished green; anything else stops the
# run (caller's decision). 'infeasible'/'dryrun' are non-fatal skips.
function Invoke-Issue {
    param([int]$IssueNum, [string]$Title, [string]$Wt, [switch]$StagedPlan)

    $issueRun = Join-Path $RunDir "issue-$IssueNum"
    New-Item -ItemType Directory -Force -Path $issueRun | Out-Null
    Log "=== #$IssueNum  $Title"

    $ralphDir = Join-Path $Wt '.ralph'
    New-Item -ItemType Directory -Force -Path $ralphDir | Out-Null
    gh issue view $IssueNum --json number,title,body,labels | Set-Content (Join-Path $ralphDir 'issue.json') -Encoding utf8
    Copy-Item (Join-Path $ScriptDir 'prompt.execute.md') (Join-Path $ralphDir 'exec.md') -Force

    # Plan fresh for every issue (fresh branch per run => no stale-plan reuse).
    $planPath = Join-Path $ralphDir 'plan.md'
    Remove-Item -LiteralPath $planPath -ErrorAction SilentlyContinue
    $planPrompt = if ($StagedPlan) { 'prompt.plan.staged.md' } else { 'prompt.plan.md' }
    Log "  planning… [$(if($StagedPlan){'staged-plan skill'}else{'standard'})]"
    Invoke-Plan -Cwd $Wt -PromptText (Get-Content (Join-Path $ScriptDir $planPrompt) -Raw) -OutLog (Join-Path $issueRun 'plan.log') -Staged:$StagedPlan

    $open = Get-OpenSteps $planPath
    if ($open -lt 0) { Log "  no plan written — skipping issue."; return 'infeasible' }
    if ($open -eq 0) { Log "  no actionable steps — infeasible, skipping issue."; return 'infeasible' }
    Copy-Item $planPath (Join-Path $issueRun 'plan.md') -Force
    Log "  plan: $open open step(s)"

    if ($DryRun) { Log "  [DryRun] plan saved to $(Join-Path $issueRun 'plan.md')."; return 'dryrun' }

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
    $before = (git -C $Wt rev-parse HEAD).Trim()
    if ($HeadlessExec) {
        $promptFile = Join-Path $issueRun 'exec-prompt.in'
        Get-Content (Join-Path $ScriptDir 'prompt.execute.md') -Raw | Set-Content $promptFile -Encoding utf8
        Log "  executing [headless -p]…"
        $status = Invoke-ExecLoop -Cwd $Wt -PlanPath $planPath -IssueRun $issueRun -PromptFile $promptFile -Model $execModel
    } else {
        Log "  executing [interactive$(if(-not $NoRemoteControl){' +remote'})]…"
        $flag = Join-Path $issueRun 'status.flag'
        $status = Invoke-Interactive -Cwd $Wt -FlagFile $flag -Model $execModel -Name "ralph-$IssueNum" `
            -InitialPrompt 'Read .ralph/exec.md and follow it exactly to implement .ralph/plan.md for this issue. Emit RALPH_DONE_EXIT when finished.'
        # Interactive sessions exit on a usage limit; detect it from the transcript.
        if ($status -in @('exited', 'timeout', 'deadline', 'unknown')) {
            $tr = Get-LatestTranscript
            if ($tr) { $txt = Get-Content $tr.FullName -Raw; if (Test-LimitText $txt) { $status = 'limit'; $script:LimitText = $txt } }
        }
    }

    $after   = (git -C $Wt rev-parse HEAD).Trim()
    $commits = @(git -C $Wt rev-list "$before..$after").Count
    Log "  execution ended: $status ($commits commit(s) this issue)"
    return $status
}

# --- Main ---------------------------------------------------------------------
Log "Ralph run $RunStamp | base=$BaseBranch deadline=$($Deadline.ToString('HH:mm')) perIssue=${MaxMinutesPerIssue}min plan=$PlanModel/$PlanEffort exec=$(if($ExecModel){$ExecModel}else{'auto'})/$ExecEffort$(if($HeadlessExec){' [headless]'}else{' [interactive]'})$(if($DryRun){' [DryRun]'})"
git fetch origin --quiet

$issues = (gh issue list --label AFK --state open --json number,title,labels --limit 100) | ConvertFrom-Json
if ($OnlyIssue -gt 0) { $issues = $issues | Where-Object { $_.number -eq $OnlyIssue } }
if (-not $issues) { Log "No open AFK issues to process. Done."; return }
# Respect task sequence: process in ascending issue-number order (#5, #6, #9 ...).
$issues = @($issues | Sort-Object number)
Log "Queue: $($issues.Count) issue(s) in order: $((($issues | ForEach-Object { '#' + $_.number }) -join ' -> '))"

# One branch per run, in a throwaway worktree off the chosen base.
$Branch = "afk/run-$RunStamp"
$Wt     = Join-Path $WtRoot "run-$RunStamp"
if (-not (git rev-parse --verify --quiet "$BaseBranch^{commit}")) { Log "! base '$BaseBranch' not found — aborting."; return }
if (Test-Path $Wt) { git worktree remove --force $Wt 2>$null }
git worktree add -b $Branch $Wt $BaseBranch --quiet
if ($LASTEXITCODE -ne 0) { Log "! could not create run worktree/branch — aborting."; return }
Log "Run branch: $Branch  (base: $BaseBranch, worktree: $Wt)"

$stopped    = $false
$stopStatus = ''
$lastIssue  = 0
try {
    foreach ($issue in $issues) {
        if ((Get-Date) -ge $Deadline) { Log "DEADLINE reached. Stopping."; $stopped = $true; $stopStatus = 'deadline'; break }

        $staged = $issue.labels.name -contains 'stagedplan'
        $status = Invoke-Issue -IssueNum $issue.number -Title $issue.title -Wt $Wt -StagedPlan:$staged
        $lastIssue = $issue.number

        if ($status -eq 'DONE' -or $status -eq 'infeasible' -or $status -eq 'dryrun') { continue }

        # Anything else is a non-green stop: hand over the branch as it stands.
        $stopped = $true; $stopStatus = $status
        if ($status -eq 'limit') {
            $reset = if ($script:LimitText) { Get-ResetDateTime $script:LimitText } else { $null }
            $when  = if ($reset) { " Resets ~$($reset.ToString('HH:mm')); re-run after that." } else { '' }
            Log "STOP: usage limit hit on #$($issue.number).$when"
        } elseif ($status -like 'BLOCKED*') {
            Log "STOP: #$($issue.number) blocked — $(($status -replace '^BLOCKED','').Trim())"
        } else {
            Log "STOP: #$($issue.number) did not finish green ($status)."
        }
        break
    }
}
catch { Log "! error on #${lastIssue}: $($_.Exception.Message)"; $stopped = $true; $stopStatus = 'error' }
finally {
    $commits = @(git -C $Wt rev-list "$BaseBranch..$Branch" 2>$null).Count
    Log "----"
    Log "Branch '$Branch' carries $commits commit(s) over $BaseBranch."
    if ($commits -gt 0) { (git -C $Wt log --oneline "$BaseBranch..$Branch") | ForEach-Object { Log "    $_" } }

    if ($stopped -or $DryRun) {
        # Keep the worktree so you can inspect / fix the stalled state in place.
        Log "Worktree kept for inspection: $Wt"
    } else {
        # Clean run: remove the worktree (the branch persists in the repo).
        git worktree remove --force $Wt 2>$null
    }
    Log "Review, then merge '$Branch' into your target (base was $BaseBranch):  git merge $Branch"
    Log "Logs: $RunDir"
}
