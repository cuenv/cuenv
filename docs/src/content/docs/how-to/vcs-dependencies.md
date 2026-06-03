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

The end-to-end sparse-subdirectory regression test keeps this example
network-free by seeding a local git source repository from the checkout's
`.agents/skills`, rewriting the example remote to that local repository, and
running `cuenv sync vcs`, `cuenv sync vcs --check`, and `cuenv task inspect`
against the temporary target module.

## Overlay Subdirectories

Set `overlay: true` when a synced subtree should share a destination directory
with repo-local content. Overlay mode materializes each immediate directory
child of `subdir` as its own managed child under `path`, and `.gitignore` tracks
those generated children individually instead of ignoring the whole parent.

```cue
vcs: "cuenv-skills": {
    url:       "https://github.com/cuenv/cuenv.git"
    reference: "0.27.1"
    vendor:    false
    subdir:    ".agents/skills"
    path:      ".agents/skills"
    overlay:   true
}
```

Requirements and behavior:

- `overlay: true` requires `subdir` and `vendor: false`.
- The overlay subtree may contain only immediate directory children. Loose files and submodules are rejected.
- Child names must be safe single path components: no spaces, control characters, glob characters, `.`, `..`, `.git`, path separators, or leading `-`.
- Cuenv writes an ownership marker into each managed child, not into the parent `path`, so hand-written siblings can live alongside synced children.
- Switching an existing managed VCS dependency at the same `path` from non-overlay to overlay is supported when the previous materialization is still unmodified.

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
