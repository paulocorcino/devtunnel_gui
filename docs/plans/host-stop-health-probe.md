# Issue #4: Host/stop group + 3-state health probe - Staged Execution Plan

<!-- scaffolded 2026-06-05 via staged-plan/lib/scaffold.py -->

## Execution model (READ FIRST)
Staged subagent execution (prompt chaining + gate checks). Do NOT run as one linear task.

0. **Pre-execution placeholder gate** (mandatory, before launching any stage). Run:
   ```
   python3 -c "import sys; sys.path.insert(0,'docs/plans'); from _verify import V; V.assert_no_placeholders('docs/plans/host-stop-health-probe.md'); sys.exit(V.summarize())"
   ```
   If non-zero, abort and surface the offending lines. Fix or delete the flagged blocks; do NOT bypass.
1. **Parent** reads this plan end-to-end (orchestration needs the full picture).
   **Subagents** read only the sections their hand-off prompt names — never
   other stages' blocks. This split is a deliberate token optimization.
2. Run Stage 0 (Pre-flight). If any gate is red on the baseline, abort.
3. For each Stage N >= 1, launch a fresh subagent (see `## Executor adapter`):
   - prompt: the verbatim Hand-off prompt block for that stage
   - description: the stage title
   - foreground, sequential, inherit model
4. On return, verify: build + gates clean, commit SHA present in `git log`,
   post-stage report written, scope respected (only declared files touched).
5. Green -> Mode handling:
   - autonomous: launch Stage N+1 immediately.
   - semi-autonomous: post the post-stage summary + `Resume? [y / edit / abort]`
     and wait. `y` -> launch Stage N+1; `edit` -> user adjusts the next
     hand-off then `y`; `abort` -> stop (committed work is preserved).
   Red -> apply the `## Execution policy` retry rule.
6. After the final stage, run `## End-to-end verification`, run the
   `## Reviewer gate` if not `none`, and emit the
   stage -> SHA -> report-path summary table.

Parent responsibilities (not delegable): launching stages in order, verifying
green between stages, running end-to-end verification, running the reviewer
gate if configured, producing the summary.

Resuming after a red stage: each hand-off prompt only assumes prior commits
exist in `git log`, not that they came from subagents. If Stage K was fixed
manually, relaunch Stage K+1 unchanged. Never re-run committed stages.

### Resource selection vocabulary (read before launching each stage)

Each stage declares `Tier:` (cognitive load) and `Effort:` (reasoning budget).
The executor at runtime maps these to the cheapest viable resource on its
platform that meets BOTH dimensions. The plan does NOT name models — that is
the executor's responsibility (it knows its own lineup and pricing).

**Tier:**
- `mechanical` — literal execution of a well-specified hand-off (rename, move,
  apply pattern from list). Smallest model that can follow the instruction.
- `standard` — typical coding within the declared file list, light judgment.
- `judgment` — scope decisions, semantic synthesis, non-obvious refactors.
- `critical` — security, public contract, data migration, irreversible changes.

**Effort:**
- `minimal` — no extended reasoning; cheapest setting.
- `standard` — default reasoning budget.
- `extended` — maximum reasoning budget the executor offers.

**Selection rule:** pick the cheapest model × reasoning combo on your platform
that meets or exceeds the declared Tier and Effort. Do NOT auto-promote on
retry — if a `mechanical` stage fails twice, the classification was wrong;
STOP and replan rather than silently escalating to a bigger model.

**Role defaults** (apply when not overridden by a stage block):
- Parent / orchestrator: `standard / standard`
- Stage 0 (pre-flight gates): `mechanical / minimal`
- Reviewer gate: `critical / extended`
- Stage N >= 1: declared per stage; absence defaults to `standard / standard`

## Execution policy (fixed defaults unless user overrode)
- Mode: autonomous
- Commit authorization: per-stage-direct
- On red: auto-retry-up-to-2 — cap of 2 retries; each retry passes the prior failure excerpt and narrows the instruction to the same file list. NEVER retry on scope violations, pre-commit hook rejections, or hook bypass attempts (escalate immediately). On exhaustion: stop and surface.
- Working-tree policy: clean-required — per-state behavior is described inline in `## Stage 0`.
- Reviewer: light  # 5 stages + host-token lifecycle handling

## Plan landing commit (mandatory before Phase 2)
Before launching Stage 1, the planner (NOT a subagent) makes a single commit
that lands this plan and its support artifacts. This is plan setup, not
feature work — isolating it here keeps Stage 0 and Stage 1+ scope-clean.

**Pre-check (mandatory):** before staging anything, inspect `.gitignore`.
The Plan landing commit assumes `docs/plans/` is **trackable**. Two cases:

- If `.gitignore` ignores `docs/plans/` wholesale (e.g. a `docs/plans/` line),
  **narrow the rule to ignore only logs**: replace that line with
  `docs/plans/logs/`. The plan file, `_verify.py`, and verify scripts MUST be
  versioned; only gate logs are excluded. Do NOT use `git add -f` to bypass —
  the rule itself needs fixing.
- If `.gitignore` does not ignore `docs/plans/`, just append `docs/plans/logs/`
  if not already present.

The landing commit MUST contain:
1. `docs/plans/host-stop-health-probe.md` — this plan file.
2. `docs/plans/_verify.py` — vendored verify primitives; the planner
   copies this from the staged-plan skill source as part of Phase 1.5 if not
   already present in the repo. Stage scripts import it via
   `sys.path.insert(0, 'docs/plans'); from _verify import V`.
3. `docs/plans/_report-template.md` — scaffolded alongside the plan;
   subagents copy it as the starting structure for post-stage reports.
4. Any `docs/plans/host-stop-health-probe-verify-stage-N.py` and
   `docs/plans/host-stop-health-probe-verify-e2e.py` scripts the plan declares.
5. `.gitignore` with the narrowed/added rule from the pre-check above
   (plus the report-ignoring pattern when report-policy = `gitignored`).

Suggested subject:
`chore(plans): land host-stop-health-probe staged plan + verify scripts`

After this commit, working tree is clean and Phase 2 starts.

## Logs policy
Gate execution logs are written to `docs/plans/logs/<prefix>-<ts>.log`
on every `run_gate()` call. They are **local evidence artifacts, not
versioned**: `docs/plans/logs/` is gitignored via the Plan landing commit.
Reports (committed alongside each stage) capture the deviations and
judgments needed for PR review; raw logs are kept locally for forensics.

## Executor adapter

Each stage runs in a fresh context window via whatever delegated-agent
mechanism the executor provides (Claude Code: `Agent` tool with
`subagent_type: general-purpose`, foreground, sequential; Codex / others: the
equivalent fresh-window mechanism, or inline in a clean session if no delegate
mechanism exists).

**Model & effort selection:** the plan declares `Tier:` and `Effort:` per stage
(see `## Execution model` § Resource selection vocabulary). The executor maps
those to its own model lineup, picking the cheapest viable combo. The plan
itself names no model — only the executor knows what's available and what it
costs.

Roles when no stage-level override is present:
- Parent / orchestrator: `standard / standard`
- Stage 0: `mechanical / minimal`
- Reviewer gate: `critical / extended`
- Stage N >= 1: as declared; default `standard / standard`

## Hand-off conventions (apply to every stage)

**Authorization:**
- MAY commit directly after all verifications pass.
- MAY NOT push.
- MAY NOT modify files outside the stage's declared file list.
- MAY NOT touch pre-existing unrelated working-tree edits.
- MAY NOT skip gates or use --no-verify / bypass hooks.
- MAY NOT spawn nested subagents (no Agent calls inside this stage).

**Scope discipline:**
- If the stage appears to require files outside the declared list, STOP and
  report. Do NOT silently expand scope.
- If pre-existing test/build failure is unrelated to this stage, STOP and
  report. Do NOT fix it.

**Failure protocol:**
- Gate fails within declared scope -> fix within scope and re-run the gate.
- Any STOP condition above -> return to parent with a clear reason.

**Return to parent:**
- Per-file summary with actual grep-found locations.
- Gate results (pass/fail + snippets).
- Commit SHA + subject.
- Deviations from the plan, if any.
- Path to the post-stage report written to disk.

## Context
Issue #4 adds a per-group **host/stop** toggle and a periodic **3-state health probe** with
per-port badges. "Stop" drops the SDK host connection but keeps the group/ports defined in the
service. The probe (GET `/`, ~5s timeout, default 60s, configurable) classifies each Public URL as
**Operational** (relay + local service ok), **Tunnel ok / service down** (relay answers, upstream
dead — 502/503 devtunnels error page), or **Down** (unreachable / not hosted).

**Why now:** #3 (manage groups/ports) is merged; hosting is the core remaining MVP capability.
**Critical constraint:** hosting (#2) exists *only* as a spike binary (`src/bin/host_spike.rs`,
`spike` feature) — it is NOT integrated into the GUI. So this track also lands the #2 integration:
the SDK host logic moves into the app behind a `TunnelHost` trait on a background tokio thread
(per ADR-0001), kept behind a new `hosting` cargo feature so the default `cargo build` stays light.

**In scope:** `hosting` feature; host engine (connect/keep-alive/reconnect/re-mint/stop); probe
engine + classifier; UI toggle + badges + i18n; HITL signature confirmation.
**Out of scope (later issues):** auto-start/auto-resume, re-login flow, settings panel, auto-renewal,
notifications. The probe interval is configurable in code (default 60s) but a settings UI is not built here.

## Alternatives considered
- **Default-on hosting (no feature flag):** rejected — forces NASM + Strawberry Perl + vendored
  OpenSSL on every `cargo build`/CI, contradicting CLAUDE.md's "spike flag gates SDK hosting from
  the main build". Chose a gated `hosting` feature with a stub for the default build.
- **`reqwest` async probe on the host tokio runtime:** rejected — pulls a large dependency tree;
  the probe is a simple blocking GET. Chose `ureq` on its own thread.
- **Fold probe-signature work into Stage 3 (no HITL stage):** rejected — the 502/503 signature must
  be confirmed against a live tunnel (issue is labelled HITL). Kept Stage 5 as the empirical gate so
  Stage 3 ships a clean `classify()` seam with provisional constants.

## Open questions
- **Tunnel ID shape:** does `devtunnel list -j` `tunnelId` already include the cluster suffix
  (`...-3000.brs`) or is cluster separate? The SDK `TunnelLocator::ID` needs `{cluster, id}`; the
  spike splits `full_id` at the last `.`. *Default assumed:* `tunnelId` carries the cluster and
  `split_locator` splits at the last `.`. *Affects:* Stage 2 (must confirm against live CLI before
  building the locator; if wrong, derive cluster from the port URI host instead).
- **Default-build toggle UX:** show the Host/Stop button disabled vs hide it entirely when built
  without `hosting`. *Default assumed:* show it `enabled: false`. *Affects:* Stage 4.

## Global conventions
- Build gate (default, light): `cargo build`
- Build gate (hosting): prepend PATH then build —
  `PATH="/c/Strawberry/perl/bin:/c/Strawberry/c/bin:/c/Users/PICHAU/AppData/Local/bin/NASM:$PATH" cargo build --features hosting`
  (the `hosting` feature compiles vendored OpenSSL → needs NASM + Strawberry Perl + MSVC).
- Lint/test gates: `cargo test` (and `cargo test --features hosting` for hosting stages),
  `cargo clippy` (+ `--features hosting`). NOTE: the baseline already has pre-existing rustfmt
  drift in `src/main.rs`, so repo-wide `cargo fmt --check` is NOT a pass/fail gate. Instead run
  `cargo fmt -- <only the files you created/edited>` so YOUR files are clean; never reformat files
  outside your stage scope.
- Invariants: English only (no Portuguese in any file); every UI string via Fluent
  (`i18n/en-US/app.ftl` → `ui/strings.slint` `Strings.*` → `apply_strings` in `main.rs`), never a
  raw literal in Rust/Slint; no hard-coded color/size in `.slint` — use `Theme.*`; SDK hosting code
  stays behind `#[cfg(feature = "hosting")]`; management calls `devtunnel.exe` directly, never PowerShell;
  default `cargo build` must keep compiling without the heavy toolchain.
- Commit style: ONE commit per stage that includes BOTH the code changes AND
  the post-stage report file. The report is staged alongside code; there is
  no separate "report commit". Trailer:
  `Co-Authored-By: $EXECUTOR_NAME $EXECUTOR_EMAIL`
  (substituted by the executor at commit time, e.g.
  `Co-Authored-By: Claude Sonnet 4.6 <noreply@anthropic.com>`).
- Report content: do NOT include the stage's own commit SHA in the report
  body (impossible: the file is part of the commit). The parent emits the
  canonical stage->SHA mapping in the End-to-end summary table.
- Staging: only files the stage declares PLUS the stage's own
  `host-stop-health-probe-stage-{N}-report.md`, by explicit path; never `git add -A`.

## Stage 0 - Pre-flight (mandatory, no feature work, no commit, no versioned report)
**Tier:** mechanical
**Effort:** minimal
Purpose: record baseline state and apply the working-tree policy so later
failures cannot be blamed on prior repo state. Plan support artifacts
(`_verify.py`, verify scripts, the plan file) are already committed via the
Plan landing commit before Phase 2 began.

**No versioned report:** Stage 0 must NOT write `host-stop-health-probe-stage-0-report.md`
under `docs/plans/` — that would leave the working tree dirty and conflict
with `clean-required`. Baseline evidence goes to the gitignored logs dir;
the human-readable summary is returned to the parent.

1. Capture `git status` and the current HEAD SHA. Write them to
   `docs/plans/logs/host-stop-health-probe-stage-0-baseline.log` (gitignored) and
   return the same summary to the parent.
2. Apply the working-tree policy from `## Execution policy`:
   - clean-required: tree must be clean; if not, abort.
   - stash-authorized: `git stash push -u -m "staged-plan-host-stop-health-probe-pre"`; record stash ref in the log + parent summary.
   - integrate-existing: leave changes in place; list them in the log + parent summary.
   - abort-until-clean: abort the plan; user resolves manually.
3. Run every gate (build, lint, tests, etc.) on the resulting baseline.
   `run_gate()` already writes its own per-command log under `docs/plans/logs/`.
4. Red -> abort. Green -> working tree must still be clean (or match the
   integrate-existing manifest); proceed to Stage 1.

<!-- BEGIN STAGE 1 -->
## Stage 1 - hosting feature + host module skeleton
**Tier:** judgment         <!-- mechanical | standard | judgment | critical — see § Resource selection vocabulary -->
**Effort:** extended       <!-- minimal | standard | extended -->
**Tier rationale:** The crux is the cfg-gating design: the host module must compile both with
and without `--features hosting` (real types vs stub), and the public type surface
(`TunnelHost` trait, `HostCommand`/`HostEvent`/`HostState`) shapes Stages 2-4. Getting these
signatures wrong cascades. No live I/O yet, but the API design needs judgment.
**Items:** S1-feature, S1-mint, S1-trait, S1-stub
**Scope:** Add the `hosting` cargo feature and a compile-both-ways `src/host` module exposing the
host control types + a shared `mint_token` helper; no hosting behavior wired yet.
**Scope discipline:** stay within the declared file list; if the stage requires
touching files outside it, STOP and report instead of silently expanding.

**Files:**
- `Cargo.toml` - add a `hosting` feature enabling `dep:tunnels`, `dep:tokio`, `dep:ureq`,
  `dep:log`; add `ureq` (with `tls`/rustls) as an optional dep. Leave the existing `spike`
  feature and the `host_spike` bin untouched. `tunnels`/`tokio` already declared optional.
- `src/devtunnel.rs` - add `pub fn mint_token(full_id: &str, scope: &str, loc: &Locale) -> Result<String>`
  lifted from `host_spike.rs::mint_token` (subprocess `devtunnel token <id> --scopes <scope> -j`,
  parse `token` field), reusing the existing `bin()` + error-string (`err-cli-*`) conventions.
  Add a `pub fn split_locator(full_id: &str) -> Option<(String, String)>` returning `(cluster, id)`
  by splitting at the last `.` (mirrors the spike) — used by Stage 2. (See Open question on id shape.)
- `src/host/mod.rs` (new) - define `pub enum HostState { Idle, Connecting, Hosting, Reconnecting, Stopped, Error(String) }`,
  `pub enum HostCommand { Host { tunnel_id: String }, Stop { tunnel_id: String } }`,
  `pub enum HostEvent { State { tunnel_id: String, state: HostState } }`, and a
  `pub trait TunnelHost { fn send(&self, cmd: HostCommand); }`. Provide
  `pub fn spawn(events: std::sync::mpsc::Sender<HostEvent>) -> Box<dyn TunnelHost>`:
  `#[cfg(feature = "hosting")]` returns a placeholder that will be replaced in Stage 2;
  `#[cfg(not(feature = "hosting"))]` returns a `NoopHost` that drops commands. Module compiles
  both ways. Declare `engine` submodule with `#[cfg(feature = "hosting")] mod engine;` (empty/stub
  file created in Stage 2 — for Stage 1 keep all logic inline in `mod.rs`, no `engine.rs` yet).
- `src/main.rs` - add `mod host;` (top, near `mod devtunnel;`). Do NOT wire callbacks yet.

**Order of operations:**
1. Edit `Cargo.toml`: add `ureq` optional dep and the `hosting` feature array.
2. Add `mint_token` + `split_locator` to `src/devtunnel.rs` (pub), with rustdoc.
3. Create `src/host/mod.rs` with the types, trait, `NoopHost`, and cfg-gated `spawn`.
4. Add `mod host;` to `src/main.rs`.
5. Run gates (both feature variants). Fix within scope.
N. Gates pass -> write the post-stage report -> stage code files AND the
   report file together -> commit. (One commit per stage; report is committed
   alongside the code.)

**Verification:**
- `cargo build` (default, no feature) — compiles; `NoopHost` path active.
- `cargo build --features hosting` with `PATH="/c/Strawberry/perl/bin:/c/Strawberry/c/bin:/c/Users/PICHAU/AppData/Local/bin/NASM:$PATH"` — compiles (this triggers the vendored-OpenSSL build).
- `cargo test` — existing `sanitize_tunnel_id` tests still pass.
- `cargo fmt -- src/host/mod.rs src/devtunnel.rs` (your files only) and `cargo clippy` — clean.

**Manual verification (if any):** none

**Post-stage report:** write `docs/plans/host-stop-health-probe-stage-1-report.md`. Copy `docs/plans/_report-template.md` as the starting structure; leave the `Commit:` slot as `_filled by parent_` — the End-to-end summary table is the canonical source for that mapping.

**Hand-off prompt for Stage 1:**
> You are executing Stage 1 of Issue #4: Host/stop group + 3-state health probe at c:/Dev/devtunnel_gui/docs/plans/host-stop-health-probe.md.
> From that plan file, read ONLY: (a) `## Execution model`, (b) `## Execution policy`,
> (c) `## Hand-off conventions`, (d) `## Global conventions`, (e) `## Critical files`,
> and (f) your own stage block between `<!-- BEGIN STAGE 1 -->` and `<!-- END STAGE 1 -->`.
> Do NOT read other stages' blocks — they are not your context. Then read
> ./CLAUDE.md for repo-wide rules. Your authoritative spec is the stage block.
>
> Repo root: c:/Dev/devtunnel_gui
> Branch: feat/4-host-stop-health-probe
> Platform: win32  (Windows: use bash syntax, forward slashes)
>
> Status: this is the first feature stage; no prior stage commits exist beyond Stage 0 baseline.
>
> Line-number hints in the plan may be stale after prior stages; grep for symbols.
>
> Your scope: Stage 1 only - hosting feature + host module skeleton. Items: S1-feature, S1-mint, S1-trait, S1-stub.
>
> Critical rules (from CLAUDE.md):
> - Build/test gates: see this plan's `## Global conventions` (default `cargo build` vs
>   the `--features hosting` build that needs NASM + Strawberry Perl on PATH).
> - Invariants: English only; every UI string via Fluent (`app.ftl` -> `Strings.*` ->
>   `apply_strings`), never a raw literal; no hard-coded color/size in `.slint` (use `Theme.*`);
>   SDK hosting stays behind the `hosting` cargo feature; management uses `devtunnel.exe`
>   subprocess, never PowerShell.
>
> Working tree: per `## Execution policy` working-tree policy = `clean-required`.
> - clean-required / stash-authorized: tree is clean at stage start; stage only
>   files YOU modify, by explicit path; never `git add -A`.
> - integrate-existing: pre-existing dirty files listed in the Stage 0 baseline
>   summary MAY be part of your declared file list; if so, stage them; otherwise
>   leave them untouched.
>
> Files to modify:
> See the **Files** list in your stage block above (authoritative).
>
> Order of operations:
> Follow the **Order of operations** in your stage block above.
> N. Gates pass -> write the post-stage report (copy `docs/plans/_report-template.md`
>    as a starting point; leave the `Commit:` slot as `_filled by parent_` —
>    the parent fills it in the End-to-end summary table)
>    -> stage code files AND the report file together by explicit path
>    -> commit with HEREDOC including the
>    `Co-Authored-By: $EXECUTOR_NAME $EXECUTOR_EMAIL` trailer.
>    (One commit per stage; report is part of that commit.)
>
> Conventions: see `## Hand-off conventions` in this plan — it covers
> Authorization, Scope discipline, Failure protocol, and Return-to-parent
> format. They apply to this stage.
>
> Begin now.

<!-- END STAGE 1 -->
---

<!-- BEGIN STAGE 2 -->
## Stage 2 - SDK host engine (connect/keep-alive/stop)
**Tier:** critical         <!-- mechanical | standard | judgment | critical — see § Resource selection vocabulary -->
**Effort:** extended       <!-- minimal | standard | extended -->
**Tier rationale:** Token lifecycle (host + manage:ports, ~24h expiry, re-mint), relay reconnect
on drop, and clean stop are all stateful long-running concerns with external I/O that are hard to
reverse and easy to get subtly wrong (leaked connections, missed reconnects). Critical.
**Items:** S2-engine, S2-connect, S2-keepalive, S2-remint, S2-stop
**Scope:** Implement the real SDK-backed host engine behind `#[cfg(feature = "hosting")]`: a
background tokio runtime that hosts/stops groups on command and reports state changes.
**Scope discipline:** stay within the declared file list; if the stage requires
touching files outside it, STOP and report instead of silently expanding.

**Files:**
- `src/host/engine.rs` (new, `#[cfg(feature = "hosting")]`) - spawn a dedicated OS thread owning a
  `tokio` current-thread runtime + an mpsc command receiver. Per `HostCommand::Host`: mint `host`
  + `manage:ports` tokens (via `devtunnel::mint_token`), build `TunnelManagementClient`
  (`Authorization::Tunnel(manage_token)`), `TunnelLocator::ID` from `devtunnel::split_locator`,
  `RelayTunnelHost::new(...).connect(host_token)`, then `add_port` for every port of the group
  (fetch ports via `devtunnel::fetch_rows` filtered by tunnel_id, or a `show`-based helper).
  Keep-alive: `tokio::select!` on the `RelayHandle` future (reconnect with backoff on completion)
  and a ~20h re-mint timer (re-mint + reconnect before the ~24h token expiry). `HostCommand::Stop`:
  drop the handle/abort the task -> emit `HostState::Stopped`. Emit `HostEvent::State` on every
  transition. Mirror the proven sequence in `host_spike.rs` (connect -> add_port).
- `src/host/mod.rs` - replace the Stage-1 placeholder `spawn` (hosting variant) with one that
  starts `engine::start(events)` and returns a `TunnelHost` whose `send` forwards to the engine's
  command channel. Keep the `NoopHost` (non-hosting) variant unchanged.

**Order of operations:**
1. Create `src/host/engine.rs`; declare `#[cfg(feature = "hosting")] mod engine;` in `mod.rs`.
2. Implement runtime thread + command loop + per-group host task (connect + add ports).
3. Add keep-alive/reconnect + token re-mint timer + stop handling, emitting `HostEvent::State`.
4. Wire the hosting `spawn` to the engine.
5. Run gates. Fix within scope.
N. Gates pass -> write the post-stage report -> stage code files AND the
   report file together -> commit. (One commit per stage; report is committed
   alongside the code.)

**Verification:**
- `cargo build` (default) — still compiles (engine excluded).
- `cargo build --features hosting` (PATH-prepended as in Stage 1) — compiles.
- `cargo clippy --features hosting` — no new warnings in `src/host/`.
- Live hosting smoke is deferred to Stage 5 (HITL) — do NOT attempt a real connect here.

**Manual verification (if any):** none (real connect validated in Stage 5)

**Post-stage report:** write `docs/plans/host-stop-health-probe-stage-2-report.md`. Copy `docs/plans/_report-template.md` as the starting structure; leave the `Commit:` slot as `_filled by parent_` — the End-to-end summary table is the canonical source for that mapping.

**Hand-off prompt for Stage 2:**
> You are executing Stage 2 of Issue #4: Host/stop group + 3-state health probe at c:/Dev/devtunnel_gui/docs/plans/host-stop-health-probe.md.
> From that plan file, read ONLY: (a) `## Execution model`, (b) `## Execution policy`,
> (c) `## Hand-off conventions`, (d) `## Global conventions`, (e) `## Critical files`,
> and (f) your own stage block between `<!-- BEGIN STAGE 2 -->` and `<!-- END STAGE 2 -->`.
> Do NOT read other stages' blocks — they are not your context. Then read
> ./CLAUDE.md for repo-wide rules. Your authoritative spec is the stage block.
>
> Repo root: c:/Dev/devtunnel_gui
> Branch: feat/4-host-stop-health-probe
> Platform: win32  (Windows: use bash syntax, forward slashes)
>
> Status: Stages 1..1 committed (confirm with `git log --oneline -1`).
> Prior stages' work is reflected in: (1) the actual code state — run
> `git log --oneline -1` and `git diff HEAD~1 HEAD --stat` if you need
> to see what changed; (2) `## Critical files` in the plan (cross-stage index);
> (3) prior stage reports under `docs/plans/<slug>-stage-K-report.md` if you
> need detail on a specific surprise or deviation. Do NOT read other stages'
> BEGIN/END blocks for prior context — git is the source of truth.
>
> Line-number hints in the plan may be stale after prior stages; grep for symbols.
>
> Your scope: Stage 2 only - SDK host engine (connect/keep-alive/stop). Items: S2-engine, S2-connect, S2-keepalive, S2-remint, S2-stop.
>
> Critical rules (from CLAUDE.md):
> - Build/test gates: see this plan's `## Global conventions` (default `cargo build` vs
>   the `--features hosting` build that needs NASM + Strawberry Perl on PATH).
> - Invariants: English only; every UI string via Fluent (`app.ftl` -> `Strings.*` ->
>   `apply_strings`), never a raw literal; no hard-coded color/size in `.slint` (use `Theme.*`);
>   SDK hosting stays behind the `hosting` cargo feature; management uses `devtunnel.exe`
>   subprocess, never PowerShell.
>
> Working tree: per `## Execution policy` working-tree policy = `clean-required`.
> - clean-required / stash-authorized: tree is clean at stage start; stage only
>   files YOU modify, by explicit path; never `git add -A`.
> - integrate-existing: pre-existing dirty files listed in the Stage 0 baseline
>   summary MAY be part of your declared file list; if so, stage them; otherwise
>   leave them untouched.
>
> Files to modify:
> See the **Files** list in your stage block above (authoritative).
>
> Order of operations:
> Follow the **Order of operations** in your stage block above.
> N. Gates pass -> write the post-stage report (copy `docs/plans/_report-template.md`
>    as a starting point; leave the `Commit:` slot as `_filled by parent_` —
>    the parent fills it in the End-to-end summary table)
>    -> stage code files AND the report file together by explicit path
>    -> commit with HEREDOC including the
>    `Co-Authored-By: $EXECUTOR_NAME $EXECUTOR_EMAIL` trailer.
>    (One commit per stage; report is part of that commit.)
>
> Conventions: see `## Hand-off conventions` in this plan — it covers
> Authorization, Scope discipline, Failure protocol, and Return-to-parent
> format. They apply to this stage.
>
> Begin now.

<!-- END STAGE 2 -->
---

<!-- BEGIN STAGE 3 -->
## Stage 3 - Health probe engine (ureq, 3-state)
**Tier:** standard         <!-- mechanical | standard | judgment | critical — see § Resource selection vocabulary -->
**Effort:** standard       <!-- minimal | standard | extended -->
**Tier rationale:** Self-contained blocking loop with a pure classifier function that is unit-
testable. The only judgment is the provisional 502/503 signature, which Stage 5 confirms
empirically — so a best-effort constant + a clean `classify()` seam is enough here.
**Items:** S3-loop, S3-classify, S3-events
**Scope:** Add a `#[cfg(feature = "hosting")]` probe engine: a background thread that periodically
GETs each hosted port's Public URL and emits a 3-state status per URL.
**Scope discipline:** stay within the declared file list; if the stage requires
touching files outside it, STOP and report instead of silently expanding.

**Files:**
- `src/probe.rs` (new, `#[cfg(feature = "hosting")]`) - define
  `pub enum ProbeState { Operational, ServiceDown, Down }` and
  `pub enum ProbeEvent { Status { tunnel_id: String, port: i32, state: ProbeState } }`.
  Spawn a thread with an mpsc command channel (`SetTargets(Vec<(tunnel_id, port, url)>)`,
  `SetInterval(Duration)`). Loop: every `interval` (default 60s) GET `/` on each target with a
  ~5s timeout via `ureq`; pass the response/error to a pure `pub fn classify(status: Option<u16>,
  body: &str) -> ProbeState`. Provisional signature (refined in Stage 5): network/timeout error or
  no response -> `Down`; HTTP 502/503 whose body contains the devtunnels relay error marker (a
  constant `RELAY_ERROR_MARKERS: &[&str]`, e.g. `"devtunnels"`/`"tunnel"` error-page text) ->
  `ServiceDown`; 2xx/other -> `Operational`. Emit `ProbeEvent::Status` after each probe. Add
  `#[cfg(test)]` unit tests for `classify` covering the 3 branches.
- `src/main.rs` - add `mod probe;` (cfg-gated). No UI wiring yet (Stage 4 owns that).

**Order of operations:**
1. Create `src/probe.rs` with types, `classify` (pure), the thread loop, and unit tests.
2. Add `mod probe;` to `src/main.rs`.
3. Run gates (incl. `cargo test --features hosting` for the `classify` tests). Fix within scope.
N. Gates pass -> write the post-stage report -> stage code files AND the
   report file together -> commit. (One commit per stage; report is committed
   alongside the code.)

**Verification:**
- `cargo build` (default) — compiles (probe excluded).
- `cargo build --features hosting` (PATH-prepended) — compiles.
- `cargo test --features hosting` — `classify` unit tests pass (Operational / ServiceDown / Down).
- `cargo clippy --features hosting` — clean.

**Manual verification (if any):** none (live probing validated in Stage 5)

**Post-stage report:** write `docs/plans/host-stop-health-probe-stage-3-report.md`. Copy `docs/plans/_report-template.md` as the starting structure; leave the `Commit:` slot as `_filled by parent_` — the End-to-end summary table is the canonical source for that mapping.

**Hand-off prompt for Stage 3:**
> You are executing Stage 3 of Issue #4: Host/stop group + 3-state health probe at c:/Dev/devtunnel_gui/docs/plans/host-stop-health-probe.md.
> From that plan file, read ONLY: (a) `## Execution model`, (b) `## Execution policy`,
> (c) `## Hand-off conventions`, (d) `## Global conventions`, (e) `## Critical files`,
> and (f) your own stage block between `<!-- BEGIN STAGE 3 -->` and `<!-- END STAGE 3 -->`.
> Do NOT read other stages' blocks — they are not your context. Then read
> ./CLAUDE.md for repo-wide rules. Your authoritative spec is the stage block.
>
> Repo root: c:/Dev/devtunnel_gui
> Branch: feat/4-host-stop-health-probe
> Platform: win32  (Windows: use bash syntax, forward slashes)
>
> Status: Stages 1..2 committed (confirm with `git log --oneline -2`).
> Prior stages' work is reflected in: (1) the actual code state — run
> `git log --oneline -2` and `git diff HEAD~2 HEAD --stat` if you need
> to see what changed; (2) `## Critical files` in the plan (cross-stage index);
> (3) prior stage reports under `docs/plans/<slug>-stage-K-report.md` if you
> need detail on a specific surprise or deviation. Do NOT read other stages'
> BEGIN/END blocks for prior context — git is the source of truth.
>
> Line-number hints in the plan may be stale after prior stages; grep for symbols.
>
> Your scope: Stage 3 only - Health probe engine (ureq, 3-state). Items: S3-loop, S3-classify, S3-events.
>
> Critical rules (from CLAUDE.md):
> - Build/test gates: see this plan's `## Global conventions` (default `cargo build` vs
>   the `--features hosting` build that needs NASM + Strawberry Perl on PATH).
> - Invariants: English only; every UI string via Fluent (`app.ftl` -> `Strings.*` ->
>   `apply_strings`), never a raw literal; no hard-coded color/size in `.slint` (use `Theme.*`);
>   SDK hosting stays behind the `hosting` cargo feature; management uses `devtunnel.exe`
>   subprocess, never PowerShell.
>
> Working tree: per `## Execution policy` working-tree policy = `clean-required`.
> - clean-required / stash-authorized: tree is clean at stage start; stage only
>   files YOU modify, by explicit path; never `git add -A`.
> - integrate-existing: pre-existing dirty files listed in the Stage 0 baseline
>   summary MAY be part of your declared file list; if so, stage them; otherwise
>   leave them untouched.
>
> Files to modify:
> See the **Files** list in your stage block above (authoritative).
>
> Order of operations:
> Follow the **Order of operations** in your stage block above.
> N. Gates pass -> write the post-stage report (copy `docs/plans/_report-template.md`
>    as a starting point; leave the `Commit:` slot as `_filled by parent_` —
>    the parent fills it in the End-to-end summary table)
>    -> stage code files AND the report file together by explicit path
>    -> commit with HEREDOC including the
>    `Co-Authored-By: $EXECUTOR_NAME $EXECUTOR_EMAIL` trailer.
>    (One commit per stage; report is part of that commit.)
>
> Conventions: see `## Hand-off conventions` in this plan — it covers
> Authorization, Scope discipline, Failure protocol, and Return-to-parent
> format. They apply to this stage.
>
> Begin now.

<!-- END STAGE 3 -->
---

<!-- BEGIN STAGE 4 -->
## Stage 4 - UI + main.rs wiring (toggle + badges)
**Tier:** judgment         <!-- mechanical | standard | judgment | critical — see § Resource selection vocabulary -->
**Effort:** extended       <!-- minimal | standard | extended -->
**Tier rationale:** Touches the threading seam (engine/probe events -> existing Slint `Timer`
pump), reconciles per-port state with `apply_rows`, and must respect the i18n + no-hard-coded-
style invariants across Rust + Slint. Several judgment calls (group-level toggle over a per-port
list; default-build behavior of the toggle).
**Items:** S4-strings, S4-toggle, S4-badge, S4-pump
**Scope:** Wire host/stop toggle per group and per-port health badges into the UI, driven by the
engine + probe channels; default (non-hosting) build shows the toggle disabled.
**Scope discipline:** stay within the declared file list; if the stage requires
touching files outside it, STOP and report instead of silently expanding.

**Files:**
- `i18n/en-US/app.ftl` - add keys: `btn-host`, `btn-stop`, `status-hosting`, `status-stopped`,
  and badge labels `badge-operational`, `badge-service-down`, `badge-down` (+ a tooltip/aria text
  if used). English values.
- `ui/strings.slint` - add matching `in property <string>` for each new key (English defaults).
- `src/main.rs` - in `apply_strings`, add `s.set_*` for each new string. Add `mod`-level channels
  for `HostEvent` + `ProbeEvent`; call `host::spawn(...)` / probe spawn; on `host`/`stop` callbacks
  send `HostCommand`; drain both event receivers inside the existing 150ms `Timer` closure
  ([main.rs:211](src/main.rs#L211)) and update per-row `status` + per-group host state. Extend
  `apply_rows` so it preserves/derives the `status` field from the latest probe state instead of the
  hard-coded `"idle"` ([main.rs:355](src/main.rs#L355)). Map `ProbeState` -> existing status ids:
  Operational->`"ok"`, ServiceDown->`"warn"`, Down->`"down"`; hosting-but-not-yet-probed->`"host"`.
- `ui/app-window.slint` - add a host/stop `TxtButton` per group (use the group's host state to pick
  `btn-host`/`btn-stop` label) wired to new `host(tunnel-id)` / `stop(tunnel-id)` callbacks; add a
  status badge next to the port using `StatusDot` + a `Pill` carrying the localized badge label.
  Add the `host`/`stop` callbacks + any new `in property` to the `AppWindow`. No hard-coded
  colors/sizes — use `Theme.*`. In the default (non-hosting) build the toggle is `enabled: false`.

**Order of operations:**
1. Add ftl keys -> `strings.slint` properties -> `apply_strings` setters (keep the 3 in lockstep).
2. Add `host`/`stop` callbacks + host-state plumbing in `app-window.slint` and `main.rs`.
3. Spawn engine + probe; drain event channels in the Timer pump; update rows/badges.
4. Reconcile `apply_rows` status mapping.
5. Run gates (both feature variants; debug build to exercise i18n key presence). Fix within scope.
N. Gates pass -> write the post-stage report -> stage code files AND the
   report file together -> commit. (One commit per stage; report is committed
   alongside the code.)

**Verification:**
- `cargo build` and `cargo build --features hosting` (PATH-prepended) — both compile.
- `cargo test` — passes (missing i18n keys panic in debug, so this catches gaps).
- `grep -n` confirms no raw UI string literal added in `main.rs`/`.slint` (all via `Strings.*`).
- `cargo clippy` — clean.

**Manual verification (if any):** Launch `cargo run --features hosting` (PATH-prepended): the
window lists ports with a status dot + badge and a Host/Stop button per group; clicking Host
flips the label to Stop. (Full live behavior confirmed in Stage 5.)

**Post-stage report:** write `docs/plans/host-stop-health-probe-stage-4-report.md`. Copy `docs/plans/_report-template.md` as the starting structure; leave the `Commit:` slot as `_filled by parent_` — the End-to-end summary table is the canonical source for that mapping.

**Hand-off prompt for Stage 4:**
> You are executing Stage 4 of Issue #4: Host/stop group + 3-state health probe at c:/Dev/devtunnel_gui/docs/plans/host-stop-health-probe.md.
> From that plan file, read ONLY: (a) `## Execution model`, (b) `## Execution policy`,
> (c) `## Hand-off conventions`, (d) `## Global conventions`, (e) `## Critical files`,
> and (f) your own stage block between `<!-- BEGIN STAGE 4 -->` and `<!-- END STAGE 4 -->`.
> Do NOT read other stages' blocks — they are not your context. Then read
> ./CLAUDE.md for repo-wide rules. Your authoritative spec is the stage block.
>
> Repo root: c:/Dev/devtunnel_gui
> Branch: feat/4-host-stop-health-probe
> Platform: win32  (Windows: use bash syntax, forward slashes)
>
> Status: Stages 1..3 committed (confirm with `git log --oneline -3`).
> Prior stages' work is reflected in: (1) the actual code state — run
> `git log --oneline -3` and `git diff HEAD~3 HEAD --stat` if you need
> to see what changed; (2) `## Critical files` in the plan (cross-stage index);
> (3) prior stage reports under `docs/plans/<slug>-stage-K-report.md` if you
> need detail on a specific surprise or deviation. Do NOT read other stages'
> BEGIN/END blocks for prior context — git is the source of truth.
>
> Line-number hints in the plan may be stale after prior stages; grep for symbols.
>
> Your scope: Stage 4 only - UI + main.rs wiring (toggle + badges). Items: S4-strings, S4-toggle, S4-badge, S4-pump.
>
> Critical rules (from CLAUDE.md):
> - Build/test gates: see this plan's `## Global conventions` (default `cargo build` vs
>   the `--features hosting` build that needs NASM + Strawberry Perl on PATH).
> - Invariants: English only; every UI string via Fluent (`app.ftl` -> `Strings.*` ->
>   `apply_strings`), never a raw literal; no hard-coded color/size in `.slint` (use `Theme.*`);
>   SDK hosting stays behind the `hosting` cargo feature; management uses `devtunnel.exe`
>   subprocess, never PowerShell.
>
> Working tree: per `## Execution policy` working-tree policy = `clean-required`.
> - clean-required / stash-authorized: tree is clean at stage start; stage only
>   files YOU modify, by explicit path; never `git add -A`.
> - integrate-existing: pre-existing dirty files listed in the Stage 0 baseline
>   summary MAY be part of your declared file list; if so, stage them; otherwise
>   leave them untouched.
>
> Files to modify:
> See the **Files** list in your stage block above (authoritative).
>
> Order of operations:
> Follow the **Order of operations** in your stage block above.
> N. Gates pass -> write the post-stage report (copy `docs/plans/_report-template.md`
>    as a starting point; leave the `Commit:` slot as `_filled by parent_` —
>    the parent fills it in the End-to-end summary table)
>    -> stage code files AND the report file together by explicit path
>    -> commit with HEREDOC including the
>    `Co-Authored-By: $EXECUTOR_NAME $EXECUTOR_EMAIL` trailer.
>    (One commit per stage; report is part of that commit.)
>
> Conventions: see `## Hand-off conventions` in this plan — it covers
> Authorization, Scope discipline, Failure protocol, and Return-to-parent
> format. They apply to this stage.
>
> Begin now.

<!-- END STAGE 4 -->
---

<!-- BEGIN STAGE 5 -->
## Stage 5 - HITL: empirical 502/503 + live end-to-end
**Tier:** critical         <!-- mechanical | standard | judgment | critical — see § Resource selection vocabulary -->
**Effort:** extended       <!-- minimal | standard | extended -->
**Tier rationale:** Requires a live `devtunnel` login + a real hosted tunnel to capture the actual
relay error signature and confirm reconnect/anonymous access end-to-end. Human-in-the-loop;
classifier constants are tuned against observed reality, not guessed. This stage gates correctness
of the whole probe feature, hence critical.
**Items:** S5-signature, S5-e2e, S5-context
**Scope:** With a human operator, host a real group via the app, observe the 3 probe states
including a downed upstream, confirm the relay 502/503 signature, tune the classifier, and record
findings.
**Scope discipline:** stay within the declared file list; if the stage requires
touching files outside it, STOP and report instead of silently expanding.

**HITL note:** this stage cannot be completed autonomously — it pauses for the human operator to
run a live session (login, host, curl, stop the local server). The agent prepares the steps,
captures the operator's observations, and applies the resulting constant changes.

**Files:**
- `src/probe.rs` - update `RELAY_ERROR_MARKERS` / the `classify` thresholds to match the empirically
  observed devtunnels 502/503 error page; adjust unit tests to encode the real signature.
- `CONTEXT.md` - replace the "Exact signature to confirm during implementation" note
  ([CONTEXT.md:47](CONTEXT.md#L47)) with the confirmed signature.

**Order of operations:**
1. Operator: `cargo run --features hosting` (PATH-prepended), Host a group, `curl` the Public URL ->
   expect 200 / Operational. Capture the badge state.
2. Operator: stop the local upstream server (leave the tunnel hosted); re-probe. Capture the raw
   HTTP status + body of the relay error page (e.g. `curl -i <url>`).
3. Operator: confirm reconnect (kill network briefly / observe relay drop -> Reconnecting -> Hosting)
   and anonymous access (URL not redirected to login).
4. Agent: encode the observed status/body markers into `classify` + tests; update `CONTEXT.md`.
5. Run gates. Fix within scope.
N. Gates pass -> write the post-stage report -> stage code files AND the
   report file together -> commit. (One commit per stage; report is committed
   alongside the code.)

**Verification:**
- `cargo test --features hosting` — updated `classify` tests pass against the real signature.
- `cargo build --features hosting` (PATH-prepended) — compiles.

**Manual verification (if any):** the full operator walkthrough in Order of operations steps 1-3 —
all three states observed in the running app, reconnect confirmed, anonymous access confirmed.

**Post-stage report:** write `docs/plans/host-stop-health-probe-stage-5-report.md`. Copy `docs/plans/_report-template.md` as the starting structure; leave the `Commit:` slot as `_filled by parent_` — the End-to-end summary table is the canonical source for that mapping.

**Hand-off prompt for Stage 5:**
> You are executing Stage 5 of Issue #4: Host/stop group + 3-state health probe at c:/Dev/devtunnel_gui/docs/plans/host-stop-health-probe.md.
> From that plan file, read ONLY: (a) `## Execution model`, (b) `## Execution policy`,
> (c) `## Hand-off conventions`, (d) `## Global conventions`, (e) `## Critical files`,
> and (f) your own stage block between `<!-- BEGIN STAGE 5 -->` and `<!-- END STAGE 5 -->`.
> Do NOT read other stages' blocks — they are not your context. Then read
> ./CLAUDE.md for repo-wide rules. Your authoritative spec is the stage block.
>
> Repo root: c:/Dev/devtunnel_gui
> Branch: feat/4-host-stop-health-probe
> Platform: win32  (Windows: use bash syntax, forward slashes)
>
> Status: Stages 1..4 committed (confirm with `git log --oneline -4`).
> Prior stages' work is reflected in: (1) the actual code state — run
> `git log --oneline -4` and `git diff HEAD~4 HEAD --stat` if you need
> to see what changed; (2) `## Critical files` in the plan (cross-stage index);
> (3) prior stage reports under `docs/plans/<slug>-stage-K-report.md` if you
> need detail on a specific surprise or deviation. Do NOT read other stages'
> BEGIN/END blocks for prior context — git is the source of truth.
>
> Line-number hints in the plan may be stale after prior stages; grep for symbols.
>
> Your scope: Stage 5 only - HITL: empirical 502/503 + live end-to-end. Items: S5-signature, S5-e2e, S5-context.
>
> Critical rules (from CLAUDE.md):
> - Build/test gates: see this plan's `## Global conventions` (default `cargo build` vs
>   the `--features hosting` build that needs NASM + Strawberry Perl on PATH).
> - Invariants: English only; every UI string via Fluent (`app.ftl` -> `Strings.*` ->
>   `apply_strings`), never a raw literal; no hard-coded color/size in `.slint` (use `Theme.*`);
>   SDK hosting stays behind the `hosting` cargo feature; management uses `devtunnel.exe`
>   subprocess, never PowerShell.
>
> Working tree: per `## Execution policy` working-tree policy = `clean-required`.
> - clean-required / stash-authorized: tree is clean at stage start; stage only
>   files YOU modify, by explicit path; never `git add -A`.
> - integrate-existing: pre-existing dirty files listed in the Stage 0 baseline
>   summary MAY be part of your declared file list; if so, stage them; otherwise
>   leave them untouched.
>
> Files to modify:
> See the **Files** list in your stage block above (authoritative).
>
> Order of operations:
> Follow the **Order of operations** in your stage block above.
> N. Gates pass -> write the post-stage report (copy `docs/plans/_report-template.md`
>    as a starting point; leave the `Commit:` slot as `_filled by parent_` —
>    the parent fills it in the End-to-end summary table)
>    -> stage code files AND the report file together by explicit path
>    -> commit with HEREDOC including the
>    `Co-Authored-By: $EXECUTOR_NAME $EXECUTOR_EMAIL` trailer.
>    (One commit per stage; report is part of that commit.)
>
> Conventions: see `## Hand-off conventions` in this plan — it covers
> Authorization, Scope discipline, Failure protocol, and Return-to-parent
> format. They apply to this stage.
>
> Begin now.

<!-- END STAGE 5 -->
---

## Reviewer gate (only if Reviewer != none)
**Tier:** critical
**Effort:** extended
After the final stage commits green:
- reviewer: light -> small subagent validates scope, diff vs. plan, gate
  results, post-stage reports, and obvious risk. Does NOT replan.
- reviewer: deep -> same plus security/perf/maintainability lens for
  stack-relevant best practices.
Reviewer returns one of: `pass`, `pass-with-notes`, `fail`, `blocked`.
Reviewer never edits code and never replans. On `fail`/`blocked`, stop and
surface to the user.
If a `reviewer` skill is available in the executor, prefer it; otherwise use
an inline QA prompt that takes the plan + diff range as input.

## Critical files (cross-stage index)
| File | Stages | Role |
|------|--------|------|
| `Cargo.toml` | 1 | `hosting` feature + `ureq` dep |
| `src/devtunnel.rs` | 1 | `mint_token`, `split_locator` helpers |
| `src/host/mod.rs` | 1, 2 | host control types, trait, `spawn` (stub→engine) |
| `src/host/engine.rs` | 2 | SDK host engine (connect/keep-alive/stop), `#[cfg(hosting)]` |
| `src/probe.rs` | 3, 5 | probe loop + `classify` (signature tuned in 5) |
| `src/main.rs` | 1, 3, 4 | module decls, callbacks, event-pump wiring |
| `i18n/en-US/app.ftl` | 4 | new strings |
| `ui/strings.slint` | 4 | new `Strings.*` properties |
| `ui/app-window.slint` | 4 | host/stop toggle + status badges |
| `CONTEXT.md` | 5 | confirmed 502/503 signature |

## End-to-end verification (after final stage)
- `cargo build` — default light build compiles (stub host path).
- `PATH="/c/Strawberry/perl/bin:/c/Strawberry/c/bin:/c/Users/PICHAU/AppData/Local/bin/NASM:$PATH" cargo build --features hosting` — hosting build compiles.
- `cargo test --features hosting` — all tests pass incl. `classify` (real signature) + `sanitize_tunnel_id`.
- `cargo fmt --check` && `cargo clippy --features hosting` — clean.
- Manual smoke (operator, from Stage 5): `cargo run --features hosting`, Host a group → badge goes
  Operational on a live local server; stop the upstream → badge goes "service down"; Stop the group →
  connection drops, definition persists in `devtunnel list`. Reconnect + anonymous access confirmed.

## End-to-end summary (parent fills after final stage)
| Stage | Title | Tier | Effort | Model used | Commit SHA | Status | Report |
|-------|-------|------|--------|------------|------------|--------|--------|
| 1 | hosting feature + host module skeleton | judgment | extended | opus | `165913b` | green | stage-1-report.md |
| 2 | SDK host engine (connect/keep-alive/stop) | critical | extended | opus | `63f83e9` | green | stage-2-report.md |
| 3 | Health probe engine (ureq, 3-state) | standard | standard | sonnet | `26261b8` | green | stage-3-report.md |
| 4 | UI + main.rs wiring (toggle + badges) | judgment | extended | opus | `f365b7b` | green | stage-4-report.md |
| 5 | HITL: empirical 502/503 + live end-to-end | critical | extended | — | _pending_ | **HITL — awaiting operator** | — |

Reviewer gate (light): **pass-with-notes** — all notes non-blocking; the broad `"tunnel"` relay
marker in `probe.rs` is flagged for Stage 5 HITL tuning.
<!-- one row per stage. `Model used` is what the executor actually selected
on its platform for the declared Tier/Effort (the executor fills this — the
plan never prescribes model names). Used post-hoc to audit whether the
platform mapping is well-calibrated. If <40% of rows are mechanical/standard,
the decomposition is suspect — too many stages classified as judgment/critical
defeats the cost-savings purpose. -->
