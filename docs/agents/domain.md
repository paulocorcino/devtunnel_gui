# Domain Docs

How the engineering skills should consume this repo's domain documentation when exploring the codebase.

## Before exploring, read these

- **`CONTEXT.md`** at the repo root — the domain glossary and resolved decisions for DevTunnel GUI.
- **`docs/adr/`** — read ADRs that touch the area you're about to work in (e.g. `0001-split-cli-management-sdk-hosting-rust.md`).

If any of these files don't exist, **proceed silently**. Don't flag their absence; don't suggest creating them upfront. The producer skill (`/grill-with-docs`) creates them lazily when terms or decisions actually get resolved.

## File structure

Single-context repo:

```
/
├── CONTEXT.md
├── docs/
│   ├── adr/
│   │   └── 0001-split-cli-management-sdk-hosting-rust.md
│   ├── agents/        ← this setup (issue-tracker, triage-labels, domain)
│   └── backlogs/      ← issues + PRDs (local-markdown tracker)
└── src/
```

## Use the glossary's vocabulary

When your output names a domain concept (in an issue title, a refactor proposal, a hypothesis, a test name), use the term as defined in `CONTEXT.md` — e.g. **Group** (= one Dev Tunnel), **Port**, **Host**, **Health probe (3 states)**, **Real Tunnel ID** vs **Requested Tunnel ID**. Don't drift to synonyms the glossary explicitly avoids (e.g. PowerShell scripting was eliminated — see the decisions section).

If the concept you need isn't in the glossary yet, that's a signal — either you're inventing language the project doesn't use (reconsider) or there's a real gap (note it for `/grill-with-docs`).

## Flag ADR conflicts

If your output contradicts an existing ADR, surface it explicitly rather than silently overriding:

> _Contradicts ADR-0001 (split CLI management / SDK hosting) — but worth reopening because…_
