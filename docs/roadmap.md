# Roadmap

Portfolio view — each row is a track. Detail (requirements, DoD, issue breakdown)
lives in the PRD. Open items do not live here.

State flows in one direction: `planned → active → done`. A track moves to `done`
when all its issues are closed and the PRD's Definition of Done is satisfied.

## Tracks

| # | Track ID | Title | Status | PRD |
|---|----------|-------|--------|-----|
| 0 | `t0` | First-run onboarding | `done` | [PRD-0003](prd/0003-t0-onboarding-first-run.md) |
| 1 | `t1` | Hosting robustness (spike) | `active` | [PRD-0004](prd/0004-t1-hosting-robustness.md) |
| 2 | `t2` | Metrics line chart | `active` | [PRD-0005](prd/0005-t2-metrics-chart.md) |
| 3 | `t3` | Pure view-fold module | `active` | [PRD-0006](prd/0006-t3-view-fold.md) |

## Updating this file

- When a PRD is created: fill in the PRD link.
- When a track becomes active: update Status to `active`.
- When all issues in a track are closed and the PRD DoD is met: update Status to `done`.
- Do not add detail here — detail belongs in the PRD.
