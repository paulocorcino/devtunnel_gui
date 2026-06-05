You are running inside an autonomous "Ralph loop". This is the PLANNING pass
for a single GitHub issue. You will NOT write production code in this pass —
you only produce a plan that a later execution loop will consume.

## Context on disk
- `.ralph/issue.json` — the GitHub issue (number, title, body, labels).
- `CLAUDE.md`, `CONTEXT.md`, `docs/adr/` — project rules and domain. Read what
  is relevant. This is a Rust + Slint desktop tray app.

## Your task
1. Read `.ralph/issue.json` and the relevant project docs.
2. Decide whether the issue is well-specified enough to implement
   autonomously, end to end, with a clear "done" criterion that `cargo test`
   (or a build) can verify.
3. Write `.ralph/plan.md` with this exact shape:

   ```
   # Plan for #<number>: <title>

   ## Feasible: yes | no
   <one or two sentences. If "no", explain what is missing — the loop will
   skip the issue and leave a comment.>

   ## Done when
   - <a concrete, checkable condition, e.g. "cargo test passes and `xyz`
     behaves as ...">

   ## Steps
   - [ ] <smallest sensible step 1 — one focused change>
   - [ ] <step 2>
   - [ ] <...>
   - [ ] cargo fmt && cargo test pass with no new warnings
   ```

## Rules
- Each step must be small enough to complete and commit in one short
  iteration. Prefer many tiny steps over a few large ones.
- The LAST step is always a green-build/test gate.
- If "Feasible: no", still write the file (with no `[ ]` steps) so the loop
  can read your reasoning. Do not invent scope the issue did not ask for.
- All text in English (project rule). Do not modify anything other than
  `.ralph/plan.md` in this pass.
- Do not run git, cargo build, or edit source files now. Just plan.
