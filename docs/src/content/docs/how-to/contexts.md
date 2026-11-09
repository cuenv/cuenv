---
title: Contexts
description: Select environment variables from the facts of an invocation
---

A context gives a variable a value only for some invocations: an infrastructure
plan, a deploy task, a pull request, a workstation. cuenv decides which contexts
are active each time it resolves an environment, so there is no flag to pass.

Use an [environment](/how-to/typed-environments/) when you choose the target
yourself (`-e production`). Use a context when the invocation decides.

## Define a context

A context is a CUE constraint, `when`, over the facts of an invocation:

```cue
contexts: {
    plan:        when: infrastructure: "plan"
    apply:       when: infrastructure: "apply" | "import" | "destroy"
    deploy:      when: task:           =~"^deploy"
    pullRequest: when: ci: event:      "pull_request"
    local:       when: ci:             null
    localPlan:   when: local.when & plan.when
}
```

Fields you leave out match anything. `when` may use literals, `=~`, `!~`,
`!=`, `|` and `&`, and may refer to another context's `when`. Any other
constraint is an error.

## The facts

| Fact             | Value                                                             |
| ---------------- | ----------------------------------------------------------------- |
| `command`        | `"task"`, `"exec"`, `"infrastructure"` or `"shell"`               |
| `task`           | The task's name, otherwise `null`                                 |
| `exec`           | The program `cuenv exec` runs, otherwise `null`                   |
| `infrastructure` | The infrastructure action, otherwise `null`                       |
| `ci`             | `null` outside CI, otherwise `{provider, event, ref, actor}`      |
| `user`           | `{name, email}` from `git config`                                 |
| `host`           | `{os, arch, name}`                                                |
| `vcs`            | `{branch}`                                                        |

Every fact is always present. One that does not apply, or that cuenv could not
detect, is `null`. `ci.provider` is `"github"`, `"gitlab"`, `"buildkite"` or
`"unknown"`; `event`, `ref` and `actor` are the provider's own names (on GitHub,
`GITHUB_EVENT_NAME`, `GITHUB_REF` and `GITHUB_ACTOR`).

There is no negation. Write the alternatives, and remember `null`:

```cue
contexts: notPullRequest: when: ci: null | {event: !="pull_request"}
```

## Set variables under a context

`env.context.<name>` mirrors `env.environment.<name>`:

```cue
env: {
    REGION: "eu-west-1"

    context: {
        plan: CLOUDFLARE_API_TOKEN:  _readOnly & {secretName: "CLOUDFLARE_READ_ONLY_TOKEN"}
        apply: CLOUDFLARE_API_TOKEN: _readWrite & {secretName: "CLOUDFLARE_READ_WRITE_TOKEN"}
        deploy: WRANGLER_TOKEN:      _wrangler
    }
}
```

`cuenv infrastructure plan` receives the read-only token, `apply` the read-write
one, and no other command receives either. A secret under a context that is not
active is never resolved.

Three rules keep the result unambiguous. cuenv reports a violation whenever it
resolves the environment, not only in the invocation that would hit it:

- A variable set under a context must not also be set in the base `env` or
  under an environment. That is what guarantees it is withheld when no context
  matches.
- Two contexts that can both be active must not set the same variable.
  `plan` and `apply` never overlap; `plan` and `local` do.
- `env.context.<name>` needs a `contexts.<name>`, and `when` may only name the
  facts above.

Precedence is base, then the selected environment, then active contexts, then
`env.context.<name>.environment.<environment>`.

[Policies](/reference/cue-schema/#policy) are unchanged: a context chooses the
value, and a policy still decides which consumer may read it.

## Contexts do not grant access

:::caution
Every fact is reported by the machine running cuenv. Anyone who can edit
`env.cue`, set a git email, or export `GITHUB_EVENT_NAME` can make a context
match.
:::

A context chooses which credential an invocation asks for. Whether it may have
it is for the secrets backend to enforce, from claims it can verify: for GitHub
Actions, an OIDC identity bound to the repository, ref, environment and event.
Give the read-only and read-write secrets separate identities there. Do not key
a privileged credential on `user`.

## Where contexts apply

`cuenv task`, `cuenv exec`, `cuenv infrastructure`, `cuenv env print` and
`cuenv ci` select context variables. For `cuenv task` and `cuenv ci` the `task`
fact is the task that was asked for. CI jobs log in to the secret providers of
every context, since the generator does not predict which will be active.

Services (`cuenv up`), shell hooks, `cuenv export` and infrastructure state
credentials do not read `env.context` yet: context variables are absent there.
