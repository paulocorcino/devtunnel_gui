# Archived — superseded by the PRD/roadmap + GitHub model

This directory held the old local-markdown issue tracker (`<feature>/PRD.md` +
`<feature>/issues/`). As of 2026-06-18 the repo adopted the standard
[setup-pocock](https://github.com/mattpocock/skills) layout:

- **Issues** now live on **GitHub** (`paulocorcino/devtunnel_gui`) — see `docs/agents/issue-tracker.md`.
- **PRDs** now live in [`docs/prd/`](../prd/), one per roadmap track, each mapped to a GitHub Milestone.
- **Roadmap** portfolio is [`docs/roadmap.md`](../roadmap.md).

## Where each old feature went

| Old `docs/backlogs/<feature>/` | PRD | GitHub Milestone | Notes |
|---|---|---|---|
| `onboarding-first-run/` | [PRD-0003](../prd/0003-t0-onboarding-first-run.md) | PRD-0003 (closed) | Shipped — issue #34 |
| `hosting-robustness/` | [PRD-0004](../prd/0004-t1-hosting-robustness.md) | PRD-0004 | Spike — issues #35–#40 |
| `metrics-chart/` | [PRD-0005](../prd/0005-t2-metrics-chart.md) | PRD-0005 | Issue #41 |
| `view-fold/` | [PRD-0006](../prd/0006-t3-view-fold.md) | PRD-0006 | Issue #42 |

The original feature folders were removed after migration; their committed history is
preserved in git. Do not add new work here — use `docs/prd/` + GitHub.
