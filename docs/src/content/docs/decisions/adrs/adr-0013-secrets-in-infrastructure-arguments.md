---
id: ADR-0013
title: Secrets in Infrastructure Resource Arguments
status: Accepted
decision_date: 2026-11-06
approvers:
  - Core Maintainers
related_features: []
supersedes: []
superseded_by: []
---

## Context

Infrastructure resource arguments could not take a `#Secret`. A secret from a
resolver reached a provider only if the provider read it from an environment
variable. Some providers can only receive a secret as an argument: copying a
Cloudflare root token from 1Password into Infisical for CI needs the
`infisical_secret` resource's `value` argument, and the Infisical provider does
not read it from the environment ([#570](https://github.com/cuenv/cuenv/issues/570)).

The workarounds all failed. Project `env` does not reach a resource argument;
cuenv has no input variables, so `TF_VAR_*` means nothing; a literal is
committed to the repository; a data source cannot map a secret manager's
nested fields onto scalar outputs; and the provider exposes no write-only
argument. The only fallback was a task that calls the secret manager outside
the plan, the state and the dependency graph.

The issue set the security requirements: resolve only for the commands that
need the value, keep it out of CUE evaluation output and the frozen
configuration, register it for redaction before any provider call, show it as
`(sensitive)` in plans, never echo resolver errors, plan a changed secret as an
update without printing either value, and document that state stores it.

## Decision

### A `#Secret` is written where the argument is

A string argument of a resource or data source can be a `#Secret` directly in
`configuration`. Input variables were considered and rejected: CUE already
gives reuse (a `let` or a hidden field holding a `#Secret` can be referenced
from many arguments), and variables would add a second binding mechanism and a
CLI override path that undercuts CUE as the source of truth.

### Classification follows the provider schema

An object at a string position is a secret when it names a `resolver`; a
string position can never legally hold an object, so this is unambiguous. The
same object at a dynamic position is data, and a provider attribute that is
itself called `resolver` is never mistaken for one. A secret is refused in a
number, a bool, a set element (a set element is identified by its value), and a
computed-only argument. Write-only handling is described in the follow-up below. Generated resource and data source types
accept `string | #Secret` at the same positions, with a structural `#Secret`
defined in each generated package so the packages keep no dependency on
cuenv's schema; the engine is the real validator.

### Resolution is lazy, once per command, and outside CUE

Checking declarations keeps each secret an unknown string. The value is
resolved only when a configuration is about to reach a provider, after CUE
specialization, and placed on the engine's value, so CUE never sees it. `plan`
and `apply` resolve the secrets of what they plan; `destroy`, `import` and the
state commands never resolve one. Each distinct secret is resolved at most
once per command, so the plan `apply` makes again under the lock sees the value
the operator confirmed, even if the secret manager rotated it meanwhile. The
value is registered for redaction before it is handed to the engine.

### No new trust gate

`allowInfrastructure` restricts the project's shared variables. A secret
written in the infrastructure configuration is declared by that configuration,
which can already run any provider binary through `path`, so an `#ExecSecret`
there grants nothing new. Resolver credentials are still withheld from
providers.

### Failures are value-free

A failed resolution names the resource, the argument and the resolver kind,
never the resolver's own message, because a failing resolver can print the
value or its credentials. It is a secret provider failure (exit code 3).

### The value is sensitive, and stored

Arguments holding a secret join the sensitive set like arguments set from a
sensitive output: plans mask them and state records them in
`sensitive_attributes`, which survives the secret's removal. Providers return
the arguments they receive, so the value is stored in plaintext in state, as
Terraform stores it. Write-only arguments, which a provider never returns, are
the way to keep it out ([#571](https://github.com/cuenv/cuenv/issues/571)).

## Consequences

- The motivating case works without a task outside the plan, and the copied
  secret participates in the dependency graph and in drift detection.
- State access is secret access. The how-to says so, and the state backend's
  credentials should be treated accordingly.
- A secret cannot be constrained or transformed by CUE (no interpolation into
  `"Bearer \(token)"`).
- Provider `configuration` does not take a `#Secret` yet. The machinery is the
  same and the value would never be stored, so it is the natural next step.
- Write-only arguments ([#571](https://github.com/cuenv/cuenv/issues/571)) reuse
  this classification and resolution through the composition described below.

## Follow-up: write-only composition (#571)

Managed string arguments marked write-only by the provider can use
`schema.#WriteOnly & <secret>`. Generated `#Value` constraints and `#WriteOnly`
compose on the native Terraform attribute, such as `value_wo`. The provider's
schema flag is authoritative; cuenv neither renames nor pairs attributes, and
ordinary fields reject the marker before resolution. This replaces 0.69.0's
limited logical aliases. Ordinary secrets retain the persisted
semantics above. Write-only slots must be null in provider state, and invalid
responses fail before recording or recovery. Companion versions remain manual;
a rotated secret alone does not trigger an update. Providers exposing write-only
fields use digest contract 2 to fence older runtimes; existing affected pins need
same-version repinning. The lockfile
format stays unchanged. See the implementation specification
in `docs/design/specs/2026-11-06-infrastructure-write-only-arguments.md`.
