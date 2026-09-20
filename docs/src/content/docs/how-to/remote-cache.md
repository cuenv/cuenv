---
title: Remote caching
description: Share cuenv's task cache between machines with a Bazel Remote Execution API server
---

cuenv's task cache speaks the [Bazel Remote Execution API v2][reapi]. Any REAPI
cache works — [bazel-remote][], [buildbarn][], BuildBuddy, NativeLink, EngFlow
and Namespace all expose the same `grpcs://` endpoint that Bazel's
`--remote_cache` takes.

[reapi]: https://github.com/bazelbuild/remote-apis
[bazel-remote]: https://github.com/buchgr/bazel-remote
[buildbarn]: https://github.com/buildbarn

## Configuration

```cue
package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
    name: "my-project"

    cache: remote: {
        endpoint: "grpcs://cache.example.com:443"
        auth: bearerTokenEnv: "CUENV_CACHE_TOKEN"
    }
}
```

Reads fall through to the remote and whatever is fetched is kept locally, so
the second read of a blob is local.

## Who is allowed to upload

`upload` defaults to `false`, and reading is always allowed. The intended
shape is **one trusted builder writes, everyone else reads**:

```bash
# On the CI builder
CUENV_REMOTE_CACHE_UPLOAD=true cuenv task build
```

:::caution[Why uploading is opt-in]
cuenv does not yet isolate a task's filesystem, so a task can read a file it
never declared and record an entry that is wrong on another machine. Locally
that is one confusing afternoon. Uploaded to a shared cache, it is everyone's.

Keep `upload` off for developer machines and untrusted builds — a fork's pull
request has no token and is read-only for free — and turn it on only for
builders you trust. Revisit this once filesystem isolation lands.
:::

## Environment overrides

CI usually should not hard-code an endpoint in CUE:

| Variable | Effect |
| --- | --- |
| `CUENV_REMOTE_CACHE` | Sets or replaces the endpoint. Empty **disables** the remote. |
| `CUENV_REMOTE_CACHE_UPLOAD` | `true`/`false`, overriding `upload`. |
| `CUENV_CACHE` | `off`, `read`, `write` — overrides every task's cache mode for one run. |

```bash
# Point at a cache the repository does not know about
CUENV_REMOTE_CACHE=grpcs://ci-cache.internal:443 cuenv task build

# Turn the remote off without editing CUE
CUENV_REMOTE_CACHE= cuenv task build
```

An unreadable `CUENV_REMOTE_CACHE_UPLOAD` is ignored rather than treated as
true — a typo must not start publishing entries to a shared cache.

## Credentials

Only the *name* of an environment variable is written in CUE, never a token:

```cue
cache: remote: {
    endpoint: "grpcs://cache.example.com:443"

    // authorization: Bearer $CUENV_CACHE_TOKEN
    auth: bearerTokenEnv: "CUENV_CACHE_TOKEN"

    // …or a provider that names its own header
    // auth: header: {name: "x-api-key", valueEnv: "CUENV_CACHE_KEY"}
}
```

If the named variable is unset, cuenv warns and continues anonymously. A
missing token degrades the cache; it does not fail the build.

Bazel's credential-helper protocol is not supported yet. Providers that issue
short-lived credentials through a helper — including Namespace — need the
token materialised into an environment variable first.

## What happens when the cache is unreachable

Nothing fails. An unreachable endpoint, a rejected handshake, or a server that
does not offer SHA-256 digests all log a warning and fall back to the local
cache. A cache is an optimization, so a dead network makes a build slower, not
broken.

The one exception is integrity: every blob fetched from a remote is
digest-checked before its bytes reach your workspace. A server that returns
the wrong content for a digest is rejected outright rather than trusted.

## What is not wired yet

- `cuenv sync ci` does not emit remote-cache configuration into generated
  workflows. Set the environment variables in your CI job yourself.
- There is no `cuenv cache` command, so there is no built-in way to inspect
  or prune either cache.
