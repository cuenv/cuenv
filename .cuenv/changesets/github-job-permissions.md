---
"cuenv": minor
---

Scope GitHub token permissions to each job's tasks and injected steps

BREAKING: pipeline/global GitHub permission maps now cap named scopes instead
of granting access to every job. Move required grants to task or contributor
`ci.github.permissions`. Contributor `ci.github.tasks` can restrict injected
steps to named tasks. Plain jobs receive only contents: read; unconditional
checks and PR writes are removed from expanded jobs. Thin jobs retain reporter
grants for checks and, when PR-triggered, comments. Bootstrap ignores pipeline maps. Trusted
publishing targets publish.crates and publish.cue and declares OIDC explicitly.
