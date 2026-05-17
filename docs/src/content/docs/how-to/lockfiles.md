---
title: Lockfiles
description: Commit flake.lock and cuenv.lock for reproducible projects
---

Lockfiles are part of the reproducibility contract. In Nix projects, commit both
`flake.lock` and `cuenv.lock`.

They have different jobs:

| File | Owner | Purpose |
| --- | --- | --- |
| `flake.lock` | Nix | Pins Nix flake inputs. |
| `cuenv.lock` | cuenv | Records cuenv-managed runtime state, VCS materialization, and other resolved external artifacts. |

`cuenv.lock` is not a replacement for `flake.lock`. For Nix runtimes, it records
that a project uses a local Nix flake and stores a digest derived from the
checked-in `flake.lock`, so `cuenv sync --check` can detect drift.

## Nix Runtime Lock State

Use a Nix runtime in `env.cue`:

```cue
package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
    name: "my-project"
    runtime: schema.#NixRuntime
}
```

Then sync:

```bash
cuenv sync
```

The resulting `cuenv.lock` contains a runtime entry similar to:

```toml
[runtimes."."]
type = "nix"
flake = "."
digest = "sha256:..."
lockfile = "flake.lock"
```

The Nix runtime lock path must be local, and the referenced `flake.lock` must be
fully locked.

## VCS Dependencies

VCS dependencies also write resolved commits to `cuenv.lock`:

```cue
vcs: {
    skills: {
        url:       "https://github.com/cuenv/cuenv.git"
        reference: "main"
        subdir:    ".agents/skills"
        vendor:    false
        path:      ".agents/skills"
    }
}
```

Update and validate the VCS lock state with:

```bash
cuenv sync vcs
cuenv sync vcs --check
```

## Check Lock Drift

Use the default sync check when you want all generated state to be current:

```bash
cuenv sync --check
```

Use the lock provider directly when you only want lockfile validation:

```bash
cuenv sync lock --check
```

Refresh resolved lock state:

```bash
cuenv sync lock
```

In CI, run `cuenv sync --check` so changes to `flake.lock`, VCS dependencies, or
other sync-managed inputs fail fast when `cuenv.lock` has not been refreshed.

## Commit Policy

Commit:

- `flake.lock`
- `cuenv.lock`
- Materialized vendored VCS snapshots when `vendor: true`

Do not commit generated non-vendored VCS checkouts. cuenv adds managed ignore
entries for those paths.

## See Also

- [Nix integration](/how-to/nix/) - Nix runtime setup
- [VCS dependencies](/how-to/vcs-dependencies/) - source dependency locking
- [Schema status](/reference/schema/status/) - runtime and sync support
