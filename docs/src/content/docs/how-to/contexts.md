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

A context is a filter. There are two kinds:

```cue
contexts: {
    plan:        schema.#When & {infrastructure: "plan"}
    apply:       schema.#When & {infrastructure: "apply|import|destroy"}
    pullRequest: schema.#When & {ci: event: "pull_request"}
    local:       schema.#When & {ci: null}
    deploy:      schema.#TaskFilter & {name: "deploy.*"}
    bun:         schema.#TaskFilter & {command: "bun"}
}
```

- `#When` matches the facts of an invocation: what is being run, where, and by
  whom.
- `#TaskFilter` matches the tasks cuenv runs, one task at a time.

A filter is data, so `cue export` shows your contexts like any other field.
Fields you leave out match anything.

### Strings match the whole value

A string in a filter is a regular expression that must match the whole value.
A string without regular expression characters therefore matches exactly
itself:

| Filter        | Matches                          |
| ------------- | -------------------------------- |
| `"bun"`       | `bun`, and not `bunx`            |
| `"bun.*"`     | anything that starts with `bun`  |
| `"bun\|bunx"` | `bun` or `bunx`                  |

This differs from CUE's `=~`, which matches anywhere in the value. Escape a
character you mean literally: `"g\\+\\+"` for `g++`. CUE's own constraint
operators (`=~`, `!=`, `|` between values) cannot be used in a filter: they are
not data, and `cue export` rejects them.

## `#When`: the facts

| Fact             | Value                                                             |
| ---------------- | ----------------------------------------------------------------- |
| `consumer`       | `"task"`, `"exec"`, `"infrastructure"` or `"shell"`               |
| `exec`           | The program `cuenv exec` runs, as written, otherwise `null`       |
| `infrastructure` | The infrastructure action, otherwise `null`                       |
| `ci`             | `null` outside CI, otherwise `{provider, event, ref, actor}`      |
| `user`           | `{name, email}` from `git config`                                 |
| `host`           | `{os, arch, name}`                                                |
| `vcs`            | `{branch}`                                                        |

A fact that does not apply, or that cuenv could not detect, is `null`, and
`null` in a filter matches exactly that. `ci.provider` is `"github"`,
`"gitlab"`, `"buildkite"` or `"unknown"`; `event`, `ref` and `actor` are the
provider's own names (on GitHub, `GITHUB_EVENT_NAME`, `GITHUB_REF` and
`GITHUB_ACTOR`). `ref` is the full ref on GitHub (`refs/heads/main`) and the
branch or tag name on GitLab and Buildkite. `exec` is the program exactly as
it is written on the command line, so `terraform` does not match
`/usr/bin/terraform`; an `allowExec` policy compares the same string.

A string for `consumer` or `infrastructure` must match at least one of the
fact's values, so `infrastructure: "aply"` is an error.

## `#TaskFilter`: the task

| Field     | Matches                                                          |
| --------- | ---------------------------------------------------------------- |
| `name`    | The task's full name, such as `deploy` or `deploy.staging`       |
| `command` | The program the task runs. A script task has none and never matches |
| `labels`  | Each entry must match one of the task's labels                   |

A task filter is checked for every task cuenv runs. With
`deploy: schema.#TaskFilter & {name: "deploy"}`, running `cuenv task deploy`
gives the `deploy` task the context's variables and withholds them from the
tasks it depends on. A task filter is never active for `cuenv exec`,
`cuenv infrastructure` or `cuenv env print`.

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
- Two contexts that set the same variable must be exclusive, and cuenv has to
  be able to prove it. It can when they need different consumers, when one
  needs a fact to be `null` and the other needs a value, and when they name
  the same fact or task field with values that cannot both match. `plan` and
  `apply` are exclusive; `plan` and `local` are not. Two expressions such as
  `"a.*"` and `"b.*"` are never proved exclusive.
- `env.context.<name>` needs a `contexts.<name>`, and a filter must name
  something: an empty one is an error.

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
`cuenv ci` select context variables. `cuenv task` and `cuenv ci` decide the
contexts for each task they run, and resolve a context's secrets only once a
task needs them. [Policies](/reference/cue-schema/#policy) are still checked
for the task that was asked for. CI jobs log in to the secret providers of
every context, since the generator does not predict which will be active.

Services (`cuenv up`), shell hooks and `cuenv export` do not read `env.context`
yet: context variables are absent there.

## State credentials

S3 infrastructure state takes credentials by context too, so a plan can read
the state with a key that cannot write it:

```cue
infrastructure: state: s3: {
	bucket: "acme-cuenv-state"
	region: "auto"

	// Used when no context below is active: keep these the weakest.
	accessKeyId:     _readOnly & {secretName: "STATE_READ_ONLY_ACCESS_KEY_ID"}
	secretAccessKey: _readOnly & {secretName: "STATE_READ_ONLY_SECRET_ACCESS_KEY"}

	context: apply: {
		accessKeyId:     _readWrite & {secretName: "STATE_ACCESS_KEY_ID"}
		secretAccessKey: _readWrite & {secretName: "STATE_SECRET_ACCESS_KEY"}
	}
}
```

While `apply` is active its credentials replace all three base credentials,
`sessionToken` included. Every other invocation, `state` subcommands among
them, uses the base credentials: the `infrastructure` fact names each action
(`"state-list"`, `"state-pull"`, `"unlock"` and so on), so a context can select
those as well. Two contexts that can be active together cannot both be named
here, and a name without `contexts.<name>` is an error.
