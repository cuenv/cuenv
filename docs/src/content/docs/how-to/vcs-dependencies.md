---
title: VCS Dependencies
description: Manage Git source dependencies with cuenv sync
---

Cuenv can manage Git dependencies from CUE configuration and materialize them with `cuenv sync vcs`.

```cue
import "github.com/cuenv/cuenv/schema"

vcs: {
    mylib: {
        url:       "https://github.com/example/mylib.git"
        reference: "main"
        vendor:    true
        path:      "vendor/mylib"
    }
}
```

Run:

```bash
cuenv sync vcs
```

The resolved commit is written to `cuenv.lock`. Commit the lockfile for reproducible checkouts.
Dependency names may contain ASCII letters, digits, `_`, `-`, and `.`, but cannot start with `.` or contain `..`.

## Vendored Snapshots

Set `vendor: true` to copy a source snapshot into the repository without `.git` metadata:

```cue
vcs: lib: {
    url:       "https://github.com/example/lib.git"
    reference: "v1.2.3"
    vendor:    true
    path:      "vendor/lib"
}
```

Vendored paths are intended to be tracked by Git.

## Local Checkouts

Set `vendor: false` for a local generated checkout:

```cue
vcs: lib: {
    url:       "git@github.com:example/lib.git"
    reference: "main"
    vendor:    false
    path:      ".cuenv/vcs/lib"
}
```

Cuenv updates `.gitignore` with a managed block for non-vendored paths.

## Sparse Subdirectories

Set `subdir` to materialize a single subdirectory of a remote repo via git's
sparse-checkout. Only that subtree is fetched and written to `path`. This is
the recommended way to pull agent skill packs out of a larger repository.

```cue
vcs: "cuenv-skills": {
    url:       "https://github.com/cuenv/cuenv.git"
    reference: "0.27.1"
    vendor:    false
    subdir:    ".agents/skills"
    path:      ".agents/skills"
}
```

Requirements and behavior:

- `vendor: true` makes the subtree a tracked snapshot. `vendor: false` writes the subtree as generated content and adds the target path to `.gitignore`.
- Sparse subdirectory materialization writes only the selected subtree at `path`; it does not leave a nested `.git` checkout behind.
- The path must be repo-relative, forward-slash separated, in canonical form (no leading/trailing whitespace, no leading/trailing slashes, no `a//b`), and contain no `.`, `..`, glob characters, or path components beginning with `-`.
- The lockfile records both the resolved commit and the subtree hash, so re-syncs are deterministic and `cuenv sync vcs --check` detects tampering of the materialized content.
- Requires git ≥ 2.27 (cone-mode sparse-checkout).

## Updating

By default, cuenv reuses the commit already recorded in `cuenv.lock`.

```bash
# Update all VCS refs
cuenv sync vcs --update

# Update one dependency
cuenv sync vcs --update lib

# Validate VCS state
cuenv sync vcs --check
```

Run `cuenv sync vcs` from the module root, or use `cuenv sync -A vcs`, when you remove dependencies so cuenv can prune stale lockfile and `.gitignore` entries across the workspace.
