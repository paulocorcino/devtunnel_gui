# PRD Index

Each PRD captures the problem, requirements, Definition of Done, and issue breakdown
for one roadmap track.

- **Roadmap** → points to PRDs (thin portfolio)
- **PRD** → 1 GitHub Milestone + N issues
- **Issue** → belongs to the milestone, body contains `Part of PRD-NNNN`
- **Milestone 100% closed = PRD DoD verified = PRD `done` = roadmap track `done`**

See [docs/agents/issue-tracker.md](../agents/issue-tracker.md) for the exact `gh` commands.

## Index

| PRD | Title | Track | Status | Milestone |
|-----|-------|-------|--------|-----------|
| [0003](0003-t0-onboarding-first-run.md) | First-run onboarding | `t0` | `done` | PRD-0003 |
| [0004](0004-t1-hosting-robustness.md) | Hosting robustness (spike) | `t1` | `active` | PRD-0004 |
| [0005](0005-t2-metrics-chart.md) | Metrics line chart | `t2` | `active` | PRD-0005 |
| [0006](0006-t3-view-fold.md) | Pure view-fold module | `t3` | `active` | PRD-0006 |

## Naming convention

```
NNNN-<trackid>-<slug>.md
```

- `NNNN` — sequential, zero-padded (shares the space with ADRs: `0001`/`0002` are ADRs, PRDs start at `0003`)
- `<trackid>` — short track identifier (`t0`, `t1`, …)
- `<slug>` — lowercase kebab-case description

## Workflow

1. `/to-prd` generates the PRD file from conversation context using `_template.md`.
2. Create the GitHub Milestone for the PRD:
   `gh api repos/paulocorcino/devtunnel_gui/milestones --method POST -f title="PRD-NNNN: Title"`
3. `/to-issues` reads the "Issue breakdown" section and creates the issues with
   `Part of PRD-NNNN` in the body, each attached to the milestone.
4. Fill in issue numbers in the PRD's issue breakdown table.
5. Execute issues (e.g. `/tdd`, then review).
6. When the milestone reaches 100% and DoD is met: close it, set PRD `status: done`,
   and update this file and `docs/roadmap.md`.
