# docs/agent-context: how to read + how to write

This directory is the **operational brain** of the project: what we are doing right now, why, and how we are verifying it.

This file is a routing table to prevent doc drift.

## Doc families (roles)

- **Axioms** (`docs/agent-context/axioms.*.toml`)

  - Immutable-ish constraints and operating principles.
  - When in doubt, these win.

- **Plan** (`docs/agent-context/plan.toml` + `docs/agent-context/current/implementation-plan.toml`)

  - `plan.toml`: big-picture task registry.
  - `current/implementation-plan.toml`: the currently active “what we are executing” plan.

- **Current execution** (`docs/agent-context/current/*`)

  - `task-list.toml`: status tracking.
  - `walkthrough.toml`: the narrative of what changed and why.

- **Research** (`docs/agent-context/research/*`)

  - Working papers: evidence, hypotheses, experiment notes.
  - Not automatically canonical; must declare status + canonical pointer.

- **Manual** (`docs/manual/*`)

  - Canonical “how the system works today”.
  - If a research doc turns into stable guidance, promote it to the manual.

- **RFCs** (`docs/rfcs/*`)
  - Immutable decision records (“the law”).

## Reading order (agent + human)

1. `docs/agent-context/axioms.workflow.toml` and `docs/agent-context/axioms.system.toml`
2. `docs/agent-context/plan.toml`
3. `docs/agent-context/current/implementation-plan.toml`
4. `docs/agent-context/current/task-list.toml` and `docs/agent-context/current/walkthrough.toml`
5. `docs/manual/README.md` (current-system canon)
6. `docs/agent-context/research/*` (topic-specific; see “canonicality” rules)

## Canonicality rules (anti-drift)

1. **One canonical document per topic**

   - Every topic should have exactly one “current canon” document.
   - All other documents must either:
     - link to the canonical doc, or
     - be explicitly historical/superseded, or
     - be deleted.

2. **Research docs must self-identify**

   - Every research doc must start with:
     - `Status: active | canonical | promoted | obsolete`
     - `Canonical: <path>` (required unless `Status: canonical`)

3. **Promotion rule**

   - If guidance becomes stable “this is how to do it”, promote it into `docs/manual/…`.
   - Leave behind a small stub in `docs/agent-context/research/…` that points to the manual.

4. **No duplicate runbooks**
   - If a doc contains procedures/commands intended for repeated use, it belongs in the manual.
   - Research can contain experiments, but must not become a second runbook.

## Template: research doc header

Copy/paste:

```
# <Title>

Status: active
Canonical: docs/agent-context/research/<canonical-doc>.md

Date: YYYY-MM-DD

## Goal

## Current facts (high confidence)

## Hypotheses (explicit)

## Repro / workflow

## Artifacts (contract)

## Next experiments

## Deletion / promotion criteria
```

## Canonical pointers (current)

- Edge/muvm investigation canon: `docs/agent-context/research/edge-muvm-pthread-create-eagain.md`
  - Scientific plan lives inside that doc under “Scientific plan (big-picture, systematic)”.
- Job-control wedge canon: `docs/agent-context/research/muvm-timeout-tty-job-control.md`
