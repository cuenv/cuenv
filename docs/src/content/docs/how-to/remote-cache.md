---
title: Remote caching
description: Share cuenv's task cache between machines with a Bazel Remote Execution API server
---

cuenv's task cache speaks the [Bazel Remote Execution API v2][reapi]. The
endpoint must provide ActionCache, ContentAddressableStorage, Capabilities, and
ByteStream services using SHA-256 digests. cuenv verifies those capabilities
during connection instead of assuming compatibility from a provider name.

[reapi]: https://github.com/bazelbuild/remote-apis

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

Reads fall through to the remote and whatever is fetched is streamed,
digest-verified, and kept locally, so the second read of a blob is local. A
remote action result is promoted into the local action cache only after all of
its streams and outputs have been verified and committed successfully.

## Uploads are currently disabled

`upload` defaults to `false`. The field and
`CUENV_REMOTE_CACHE_UPLOAD` override are reserved for the eventual trusted
builder flow, but cuenv currently forces the CLI connection to read-only even
when either setting requests uploads.

:::caution[Why uploads remain disabled]
The default `"dir"` sandbox isolates relative workspace access, but it is not
an OS security boundary: a command can still read absolute host paths or use
the network. Publishing such a result could turn one machine's undeclared
dependency into a shared wrong answer. Remote uploads will be enabled only
after a strict platform sandbox confines those accesses.
:::

## Environment overrides

CI usually should not hard-code an endpoint in CUE:

| Variable | Effect |
| --- | --- |
| `CUENV_REMOTE_CACHE` | Sets or replaces the endpoint. Empty **disables** the remote. |
| `CUENV_REMOTE_CACHE_UPLOAD` | Reserved `true`/`false` override. `true` currently warns and remains read-only. |
| `CUENV_CACHE` | `off`, `read`, `write`, or `read-write` — overrides every task's cache mode for one run. |

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

If the named variable is unset, cuenv warns and may continue with anonymous
reads, but configured authentication never silently becomes an anonymous
writer. A missing token degrades the cache; it does not fail the build.

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
