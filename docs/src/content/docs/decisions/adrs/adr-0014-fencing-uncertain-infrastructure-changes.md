---
id: ADR-0014
title: Fencing Uncertain Infrastructure Changes and Reviewed Plans
status: Accepted
decision_date: 2026-11-08
approvers:
  - Core Maintainers
related_features: []
supersedes: []
superseded_by: []
---

## Context

A review of `cuenv infrastructure` found that some outcomes of a provider
change were invisible to every runner but the one that made them:

- When a provider changed an object but the state write failed, the record was
  saved only to a local recovery file. Once the lock was released, another
  machine could plan against state that did not know the change and repeat it.
- A lost provider response, a killed run, or a response cuenv refused left no
  record anywhere.
- With object-storage state, recovery files were keyed by module, project and
  environment, but state is also keyed by CUE package (ADR-0012). One package
  could recover another package's change into its own state.

The same review found that the PR plan in generated workflows is only a
preview: the apply on `main` plans again, against state, data and
configuration that may have changed since the review.

## Decision

### In-flight markers in the shared backend

Before every provider mutation, the engine writes a marker record under the
run's lock, and deletes it once the outcome is recorded. Plans, applies and
destroys refuse while a marker exists.

- **A marker is an ordinary record** of the reserved resource type
  `cuenv_inflight`, named `<type>-<name>` (resource types never contain `-`).
  No backend schema changes, and no format version is raised: SQL backends
  and object storage fence it with the lock like any other write. A build that
  predates markers fails on the unknown record instead of ignoring it.
- **It holds no state values**: only the target address, the step, the start
  time and the lock identifier. The provider's result stays in the local
  recovery file, which can hold secrets and never leaves the machine.
- **Recording the outcome clears it**: `state recover` (on the machine that
  saved the change), `import` of the object, or `state remove` of the marker
  after checking the object by hand.
- **Cost**: two extra state writes per change, paced like any other write on
  R2.

Rejected: a new SQL table and an object-store field. Both raise the storage
format for every project sharing the backend (the coupling ADR-0012 removed),
and an older build would ignore them silently.

### Recovery scoped by package

A state store reports the package it keys state by (`recovery_scope`). Saved
files record it; listing skips another package's files, so they neither block
nor get recovered by it. A file without a package, written before this,
needs `--accept-backend` in a package-scoped backend.

### Reviewed plan digest

`plan` prints a digest of a redacted, compact rendering of every change to real
infrastructure, bound to the tenant. That rendering is fixed and separate from
the diff `plan` displays, so a change to the display never changes a digest. It excludes record versions, evaluator
bindings and refresh-only work, so it is stable across runs, and it is
computed from redacted text, so it never fingerprints a secret.
`apply --expect-plan <digest>` refuses a plan made under the lock with
another digest. Generated workflows can require it
(`apply.requireReviewedPlan`): the apply reads the digest from the merged
pull request's plan comment for that pull request's final head.

Rejected: storing plan files as artifacts. Plans hold sensitive values even
when their display is masked, so they would need secret storage, and a saved
plan goes stale as soon as state moves.

## Consequences

- No runner can repeat a change whose outcome another runner never recorded.
- An operator must clear a marker after an uncertain outcome. The error names
  each marker and the commands that clear it.
- A change to a secret value alone does not change the reviewed digest;
  reviewers never saw the value.
- Data that changes between the review and the apply (drift, data sources)
  makes a required reviewed plan refuse; the fix is a fresh review.
