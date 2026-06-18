# Triage Labels

The skills speak in terms of five canonical triage roles. This file maps those roles
to the actual GitHub label strings used in this repo. The repo predates the canonical
names and already uses `AFK` / `HITL` across its issue history, so those are kept and
mapped rather than renamed.

| Canonical role (mattpocock/skills) | Label in our tracker | Meaning                                  |
| ---------------------------------- | -------------------- | ---------------------------------------- |
| `needs-triage`                     | `needs-triage`       | Maintainer needs to evaluate this issue  |
| `needs-info`                       | `needs-info`         | Waiting on reporter / live observation   |
| `ready-for-agent`                  | `AFK`                | Fully specified, an agent can pick it up |
| `ready-for-human`                  | `HITL`               | Requires human-in-the-loop work          |
| `wontfix`                          | `wontfix`            | Will not be actioned                     |

When a skill mentions a role (e.g. "apply the AFK-ready triage label"), apply the
corresponding GitHub label from this table with `gh issue edit <n> --add-label "..."`.

## Notes

- `AFK` (= "can be done without interaction") and `HITL` (= "requires human
  interaction") are the project's long-standing names for `ready-for-agent` /
  `ready-for-human`. Keep using them so the label set stays consistent with the
  existing closed issues.
- `stagedplan` is an extra repo label (execute the issue with the stagedplan skill);
  it is orthogonal to triage state.
- Edit the right-hand column if the vocabulary ever changes.
