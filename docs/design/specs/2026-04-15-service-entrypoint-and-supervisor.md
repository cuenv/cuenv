# Service Entrypoint, Process Supervisor, and Environment Propagation

**Date:** 2026-04-15
**Status:** Approved

Three related improvements to `cuenv up` and the service schema:

1. Extract `#Command` and `#Script` as base types; add `#Service.entrypoint`
2. Make `cuenv up` a proper process supervisor with reaping
3. Fix environment variable and secret propagation for services

## 1. Schema: `#Command`, `#Script`, and `Service.entrypoint`

### Problem

Services and tasks duplicate the same execution fields (`command`, `args`, `script`, `scriptShell`, `shellOptions`). Users defining a service that runs the same thing as a task must repeat themselves:

```cue
services: {
    server: {
        command: "cargo"
        args: ["run", "--bin", "waddle-server"]
        // ...
    }
}
tasks: {
    dev: {
        command: "cargo"
        args: ["run", "--bin", "waddle-server"]
        // ...
    }
}
```

### Design

**Approach C: flat composition for tasks, `entrypoint` only on services.**

#### New base types (`schema/execution.cue`)

```cue
#Command: {
    command: string
    args?: [...(string | #TaskOutputRef)]
}

#Script: {
    script: string
    scriptShell?: #ScriptShell | *"bash"
    shellOptions?: #ShellOptions
}
```

#### `#Task` embeds `#Command | #Script` (fields stay flat)

```cue
#Task: {
    { #Command } | { #Script }
    env?: [string]: #EnvironmentVariable | #TaskOutputRef
    dir?: string
    hermetic?: bool | *true
    dependsOn?: [...(#TaskNode | #ContainerImage)]
    // ...all other task fields unchanged
}
```

Existing task definitions continue to work unchanged. Mutual exclusivity of `command` vs `script` is now enforced structurally by CUE's disjunction, replacing the Rust-side validation.

#### `#Service` gains `entrypoint`, loses execution fields

```cue
#Service: {
    entrypoint: #Task | #Script | #Command

    // Removed: command, args, script, scriptShell, shellOptions
    // Removed: mutual exclusivity guard (now structural)

    // Retained: all service-specific fields
    env?: [string]: #EnvironmentVariable | #TaskOutputRef
    dir?: string
    dependsOn?: [...(#TaskNode | #Service | #ContainerImage)]
    readiness?: #Readiness
    restart?: #RestartPolicy
    watch?: #Watch
    logs?: #ServiceLogs
    shutdown?: #Shutdown
    timeout?: string
    labels?: [...string]
    description?: string
    runtime?: #Runtime
}
```

#### Usage examples

Reuse a task definition:

```cue
services: {
    server: #Service & {
        entrypoint: tasks.dev
        readiness: { kind: "port", port: 5222 }
        logs: color: "magenta"
    }
}
```

Inline command:

```cue
services: {
    server: #Service & {
        entrypoint: { command: "cargo", args: ["run", "--bin", "server"] }
        readiness: { kind: "port", port: 5222 }
    }
}
```

Inline script:

```cue
services: {
    server: #Service & {
        entrypoint: {
            script: """
                cargo run --bin server
                """
        }
        readiness: { kind: "port", port: 5222 }
    }
}
```

### Rust changes

New types in `crates/core/src/manifest/`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Command {
    pub command: String,
    #[serde(default)]
    pub args: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Script {
    pub script: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script_shell: Option<ScriptShell>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell_options: Option<ShellOptions>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum Entrypoint {
    Task(Box<Task>),  // most specific first
    Script(Script),
    Command(Command),
}
```

`Service` struct drops `command`, `args`, `script`, `script_shell`, `shell_options` and gains `entrypoint: Entrypoint`.

`ServiceSupervisor::resolve_command()` matches on `self.service.entrypoint` instead of inspecting individual fields.

`ContributorEngine` (contributors.rs:79) extracts the command from `service.entrypoint` for workspace detection.

### Affected files

| File | Change |
|------|--------|
| `schema/execution.cue` | New file: `#Command`, `#Script` |
| `schema/tasks.cue` | `#Task` embeds `{ #Command } \| { #Script }` |
| `schema/services.cue` | Replace execution fields with `entrypoint` |
| `crates/core/src/manifest/mod.rs` | Add `Command`, `Script`, `Entrypoint`; rewrite `Service` |
| `crates/services/src/supervisor.rs` | `resolve_command()`, `command_display()` match on `entrypoint` |
| `crates/core/src/contributors.rs` | Extract command from `entrypoint` |
| `examples/` | Update any service definitions |
| `docs/` | Update service configuration docs |

---

## 2. Process Supervisor with Reaping

### Problem

When `cuenv up` receives SIGKILL, all spawned service processes are orphaned. Each service runs in its own process group (`setpgid(0, 0)`), isolated from cuenv's group. The Ctrl+C handler only fires on catchable signals — SIGKILL bypasses it entirely.

### Design

#### Linux: `PR_SET_PDEATHSIG`

Add `prctl(PR_SET_PDEATHSIG, SIGKILL)` in the child's `pre_exec` hook. The kernel delivers SIGKILL to the child when its parent process dies, regardless of how the parent died.

```rust
#[cfg(target_os = "linux")]
unsafe {
    cmd.pre_exec(|| {
        libc::setpgid(0, 0);  // keep per-service groups for targeted stop
        libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
        Ok(())
    });
}
```

#### macOS: `cuenv __supervise` wrapper

macOS has no `PR_SET_PDEATHSIG`. cuenv uses itself as a thin process babysitter via a hidden subcommand:

```
cuenv __supervise <command> [args...]
```

The wrapper:
1. Records the parent PID
2. Spawns the service command as a child process (with `setpgid(0, 0)`)
3. Monitors parent PID via `kqueue` with `EVFILT_PROC` + `NOTE_EXIT`
4. Forwards signals (SIGTERM, SIGINT, etc.) to the child's process group
5. On parent death, kills the child's process group and exits

#### Zombie reaping

On Linux, cuenv sets itself as a subreaper:

```rust
#[cfg(target_os = "linux")]
unsafe {
    libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1);
}
```

A background tokio task reaps zombies:

```rust
tokio::spawn(async {
    loop {
        unsafe { libc::waitpid(-1, std::ptr::null_mut(), libc::WNOHANG); }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
});
```

On macOS, the `__supervise` wrapper handles reaping for its own subtree.

#### Per-service stop is preserved

`setpgid(0, 0)` is retained so that `cuenv stop <service>` can target a single service's process group via `kill(-pgid, sig)`. The parent-death mechanism is the fallback for ungraceful death of cuenv itself.

### Affected files

| File | Change |
|------|--------|
| `crates/services/src/supervisor.rs` | Add `PR_SET_PDEATHSIG` in `spawn_process()` pre_exec |
| `crates/cuenv/src/commands/up.rs` | Set `PR_SET_CHILD_SUBREAPER`, start reap loop |
| `crates/cuenv/src/commands/mod.rs` | Register hidden `__supervise` subcommand |
| `crates/cuenv/src/commands/supervise.rs` | New file: `__supervise` implementation with kqueue |
| `crates/services/src/supervisor.rs` | On macOS, spawn via `cuenv __supervise` wrapper |

---

## 3. Environment and Secret Propagation for `cuenv up`

### Problem

`cuenv -e test up` does not propagate environment variables or secrets to services. Three gaps:

1. **`UpOptions` has no `environment` field** — the `-e` flag is never wired through to the controller or supervisors.
2. **`Service.env` is `HashMap<String, serde_json::Value>`** — raw JSON, not the `EnvValue` type that supports secrets and interpolation.
3. **`supervisor.rs::spawn_process()` calls `.as_str()` on raw JSON** — completely skips the 3-phase secret resolution pipeline that `cuenv exec` and task execution use (collect, resolve, reassemble).

### Design

#### Wire `-e` through to services

Add `environment_override: Option<String>` to `UpOptions`. Pass it through `ServiceController` to each `ServiceSupervisor`.

#### Use `EnvValue` for service env

Change `Service.env` from `HashMap<String, serde_json::Value>` to `HashMap<String, EnvValue>` — the same type tasks use. This enables secret resolution, interpolation, and policy filtering.

#### Resolve env vars in `spawn_process()`

Add `Environment::resolve_for_service_with_secrets()` in `crates/core/src/environment.rs`. It follows the same 3-phase pipeline as task resolution:

1. **Collect:** Non-secret vars pass through; secret parts are collected
2. **Resolve:** `SecretRegistry` spawns async tasks for all secrets in parallel via `JoinSet`
3. **Reassemble:** Final values reconstructed from resolved secrets + literals

Call this in `supervisor.rs::spawn_process()` before applying env vars to the child process.

#### Register secrets for redaction

After resolution, call `cuenv_events::register_secrets()` with resolved secret values so they are redacted from service log output.

### Affected files

| File | Change |
|------|--------|
| `crates/cuenv/src/commands/up.rs` | Add `environment_override` to `UpOptions`, pass project env to controller |
| `crates/services/src/controller.rs` | Accept and forward environment + resolved env to supervisors |
| `crates/services/src/supervisor.rs` | Call resolution pipeline in `spawn_process()` |
| `crates/core/src/manifest/mod.rs` | Change `Service.env` type to `HashMap<String, EnvValue>` |
| `crates/core/src/environment.rs` | Add `resolve_for_service_with_secrets()` |

---

## Implementation order

1. **Schema extraction** (`#Command`, `#Script`, `entrypoint`) — foundational, changes the types everything else depends on
2. **Environment propagation** — fixes the broken `-e` flag, changes `Service.env` type which overlaps with the schema work
3. **Process supervisor** — independent of the other two, can be done last
