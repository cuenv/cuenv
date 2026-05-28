---
title: Run services
description: Declare your dev stack once and let cuenv supervise it with readiness gating, restart policies, and file watching.
---

You already declare your environment and tasks in CUE. Services are the missing third piece: the long-running processes you actually develop against — Postgres, an API, a worker. Declare them once and `cuenv up` brings the whole stack online in dependency order, waits for each process to become *ready* (not just *started*), tails the logs, and tears everything down cleanly on `Ctrl+C`. No `docker-compose.yml` drifting away from your real config, no hand-rolled `wait-for-it.sh` — the same typed configuration that powers `cuenv exec` and `cuenv task` also powers your dev stack.

:::note[Status: partial]
Services are a **partial** feature. `cuenv up`, `ps`, `down`, `restart`, and `logs --follow` work today over persisted session state, and readiness probes, restart policies, file watching, and shutdown control are all live. The known gaps are in `dependsOn`: image-backed dependencies fail fast (see [dependsOn semantics](#dependson-semantics)). Always check [Schema status](/reference/schema/status/) before relying on a capability.
:::

## A minimal working example

Everything below is derived from the real [`examples/services-readiness`](https://github.com/cuenv/cuenv/tree/main/examples/services-readiness) example, which you can run from a checkout of the repository.

A service needs two things: an `entrypoint` that says how to run the process, and (almost always) a `readiness` probe that tells cuenv when the process is actually usable. Here is a single TCP service that listens on a port, with cuenv gating readiness on that port becoming connectable:

```cue
package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "dev-stack"

services: {
    port: schema.#Service & {
        entrypoint: schema.#Command & {
            command: "python3"
            args: [
                "-c",
                "import socket, time; s=socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1); s.bind(('127.0.0.1', 18080)); s.listen(1); time.sleep(3600)",
            ]
        }
        readiness: {
            kind: "port"
            port: 18080
        }
        shutdown: {timeout: "2s"}
    }
}
```

The `entrypoint` accepts a `#Command` (inline command + args), a `#Script`, or a full `#Task` (to reuse a task you have already defined). CUE's disjunction enforces that you pick exactly one shape.

Bring it up:

```bash
cuenv up --path examples/services-readiness --package examples
```

cuenv evaluates the CUE, discovers the services, and supervises them until you press `Ctrl+C`.

## Lifecycle walkthrough

The supervisor runs in the foreground under `cuenv up`. The other commands talk to that running session over persisted state under `.cuenv/run/<project>/`, so run them from a second terminal in the same project directory.

### Start the stack

```bash
cuenv up --path examples/services-readiness --package examples
```

`cuenv up` emits progress as it evaluates and starts services, then stays attached:

```text
cuenv up: evaluating services in examples/services-readiness (package: examples)
cuenv up: running service task dependencies: prepare
cuenv up: starting 5 service(s): port, http, log, command, delay
```

Start a subset by naming services, or filter by label:

```bash
# Only the services you need right now
cuenv up port http --path examples/services-readiness --package examples

# All services carrying a label
cuenv up --label backend --path examples/services-readiness --package examples
```

### Inspect what is running

```bash
cuenv ps --path examples/services-readiness --package examples
```

`cuenv ps` reads the session state and prints a table:

```text
NAME                 STATE        UPTIME       RESTARTS   PID
--------------------------------------------------------------
port                 ready        12s          0          48213
http                 ready        12s          0          48214
log                  ready        12s          0          48215
```

Use `-o json` for machine-readable output.

### Tail the logs

```bash
cuenv logs -f --path examples/services-readiness --package examples
```

Logs are line-prefixed with the service name. `-f`/`--follow` streams appended lines until the active `cuenv up` session exits; `-n`/`--lines` controls how much history prints first (default `100`). Name services to scope the output:

```bash
cuenv logs http log --path examples/services-readiness --package examples
```

### Restart a service

```bash
cuenv restart log --path examples/services-readiness --package examples
```

`cuenv restart` requires an active session. It queues a persisted restart request for each named service, and the running supervisor consumes it to stop and re-spawn that service.

### Bring it all down

```bash
cuenv down --path examples/services-readiness --package examples
```

`cuenv down` signals the running supervisor to shut down gracefully. Omit service names to stop the whole session; name services to stop just those. Pressing `Ctrl+C` in the `cuenv up` terminal does the same thing for the entire session.

## Readiness probes

A service is not "up" until it is *ready*. The `readiness` block tells cuenv how to decide. There is one probe per service in v1, chosen by `kind`. The probe runs repeatedly until it succeeds or the timeout elapses.

The first four kinds share these timing fields (defaults shown): `interval` (`500ms`) between attempts, `timeout` (`60s`) before the service is considered failed, and `initialDelay` (`0s`) before the first attempt.

**`port`** — wait for a TCP port to accept a connection. Defaults to `127.0.0.1`; override with `host`.

```cue
readiness: {
    kind: "port"
    port: 18080
}
```

**`http`** — issue an HTTP request and check the status code. `url` is required. `expectStatus` defaults to the 2xx family (`[200, 201, 202, 203, 204, 205, 206]`); `method` is `GET` or `HEAD` (default `GET`).

```cue
readiness: {
    kind:         "http"
    url:          "http://127.0.0.1:18081/healthz"
    expectStatus: [200]
    method:       "GET"
}
```

**`log`** — declare ready when a regex first matches a log line. `pattern` is required; `source` is `stdout`, `stderr`, or `either` (default `either`).

```cue
readiness: {
    kind:    "log"
    pattern: "worker ready"
    source:  "stdout"
}
```

**`command`** — run a command in the service's environment; exit `0` means ready. The probe runs as a separate process, not the service itself.

```cue
readiness: {
    kind:    "command"
    command: "test"
    args: ["-f", ".cuenv-command-ready"]
}
```

**`delay`** — a dumb sleep. An escape hatch only, for processes with no observable readiness signal. `delay` is required; this kind does not take the shared timing fields.

```cue
readiness: {
    kind:  "delay"
    delay: "1s"
}
```

## Restart policy, file watching, shutdown, and logs

### Restart policy

`restart` controls what happens when a service exits. `mode` defaults to `onFailure`:

- `never` — a crash marks the service failed and aborts its dependents.
- `onFailure` — restart on a non-zero exit (the default).
- `always` — restart on any exit.
- `unlessStopped` — like `always`, but a `cuenv down <svc>` is sticky and the supervisor will not bring it back.

`backoff` applies exponential delay between restarts (`initial` `1s`, `max` `30s`, `factor` `2.0`). `maxRestarts` (default `5`) caps restarts within a sliding `window` (default `60s`); exceeding the cap marks the service failed and aborts dependents.

```cue
restart: {
    mode:        "unlessStopped"
    maxRestarts: 3
    window:      "30s"
    backoff: {initial: "1s", max: "30s", factor: 2.0}
}
```

### File watching

`watch` re-runs the service when files change — the fast inner loop for local development. `paths` (glob patterns relative to the project root) is required; `ignore` uses gitignore syntax; `debounce` (default `200ms`) batches rapid changes. `on` supports `restart` only in v1. Use `rebuild` to run tasks (for example, recompile a binary) before the restart.

```cue
watch: {
    paths: ["src/**/*.go"]
    ignore: ["**/*_test.go"]
    debounce: "300ms"
    on: "restart"
}
```

### Shutdown

`shutdown` controls graceful teardown. `signal` defaults to `SIGTERM` (use `SIGINT`, `SIGHUP`, or `SIGQUIT` for stubborn programs); `timeout` (default `10s`) is the grace period before cuenv sends `SIGKILL`.

```cue
shutdown: {
    signal:  "SIGTERM"
    timeout: "2s"
}
```

### Logs

`logs` shapes the multiplexed output. `prefix` defaults to the service name; `color` is an optional ANSI hint the renderer may honour. `persist` defaults to `true`, writing each service's output to `.cuenv/run/<project>/logs/<svc>.log` — which is exactly what `cuenv logs` reads.

```cue
logs: {
    prefix:  "log-probe"
    color:   "cyan"
    persist: true
}
```

## dependsOn semantics

`dependsOn` mixes three kinds of dependency, and each behaves differently. Be precise here, because the third is a known gap.

- **Service dependencies** wait for *readiness*. If `api` depends on `db`, cuenv starts `db`, waits for its readiness probe to pass, and only then starts `api`. Starting a selected service also pulls in its service dependencies automatically.
- **Task dependencies** run to *completion* before service startup. cuenv builds a task graph from the referenced tasks and runs it before any supervised process spawns — handy for migrations or seed data. In the example, the `port` service depends on the `prepare` task:

  ```cue
  services: {
      port: schema.#Service & {
          dependsOn: [tasks.prepare]
          // ...
      }
  }
  ```

- **Image dependencies** are recognized in the dependency plan, but **fail fast today**. If a selected service depends on a `#ContainerImage`, `cuenv up` aborts before running any task roots with an error explaining that image execution backends are not yet wired into service startup. This is intentional — it avoids build side effects that cannot be consumed. Track progress on [Schema status](/reference/schema/status/).

## Troubleshooting and where to go next

**Readiness timeout.** If a service never reaches ready, the probe is failing. For `http`, confirm the `url` and that the process is bound to the address the probe checks. For `port`, the probe defaults to `127.0.0.1`; a process bound only to a container or external interface will not be seen. Raise `timeout` or set `initialDelay` for slow starters, and use `cuenv logs` to read what the process actually printed.

**"Cannot follow logs" / "no active session".** `cuenv logs -f`, `cuenv restart`, and `cuenv down` all require a live `cuenv up` session. Start `cuenv up` in one terminal first, then run these from a second terminal in the same project directory so they find the session state under `.cuenv/run/<project>/`.

**Image dependency error on `up`.** As above, image-backed `dependsOn` fails fast by design. Until backends land, build images separately with [`cuenv build`](/reference/cli/#cuenv-build) and reference the resulting process directly.

Next steps:

- [CLI reference](/reference/cli/) — every flag for `up`, `down`, `ps`, `logs`, and `restart`.
- [`cuenv build`](/reference/cli/#cuenv-build) — building container images, and the path toward image-backed services.
- [Schema status](/reference/schema/status/) — the authoritative source for what is implemented, partial, or schema-only.
- [Run tasks](/how-to/run-tasks/) — define the tasks you wire into `dependsOn`.
- [Secrets](/how-to/secrets/) — secrets in a service's `env` are resolved at runtime and redacted from logs.
</content>
</invoke>
