# VCS Subdir: Sparse Materialization of a Single Directory

Date: 2026-05-12
Status: Design

## Goal

Let a `#VcsDependency` materialise a single subdirectory of a remote git repository at a pinned commit, rather than the whole repo. This makes "fetch `.agents/skills/` from `cuenv/cuenv` at a tagged release" a one-entry VCS recipe, and incidentally enables partial materialization of any monorepo subtree.

## Non-goals

- No `skills:` schema block. Skill packs are vendored via VCS like any other directory dependency; documentation provides the canonical recipe.
- No binary-bundled assets. cuenv does not ship skill content inside the cuenv binary.
- No multi-tool target translation. Skills land at `.agents/skills/<pack>/SKILL.md` and nowhere else.
- No nested sparse checkout left behind at the destination. Subdir materialization always writes the selected subtree without `.git`; `vendor` only decides whether the destination is tracked or ignored by the outer repo.

## Background

Today, `#VcsDependency` clones a whole repo at a pinned commit and materialises the entire tree at `path`. The lockfile (`cuenv.lock`) records the resolved `commit` and root `tree` hashes, and `cuenv sync vcs` reuses them deterministically unless `-u` is passed. An ownership marker file (`.cuenv-vcs`) at the install root holds the commit SHA so subsequent syncs detect tampering.

This works fine for "vendor a small library." It is wasteful for "vendor a single subdirectory of a large repo" — the entire history blobs and the entire tree both have to land on disk before the irrelevant parts are discarded.

## Schema change

`schema/vcs.cue`:

```cue
#VcsDependency: close({
    url!: string
    reference: string | *"HEAD"
    vendor!: bool
    path!: string
    // When set, only this subtree of the repo is materialised at `path`.
    // Must be a relative, forward-slash path inside the repo containing only
    // literal safe path components (no ".", "..", glob characters, control
    // characters, or backslashes).
    subdir?: string
})
```

The CUE-side validation is intentionally lax (just a regex would be the right tightening if cuengine surfaces a good error) — the authoritative validation runs in Rust at sync time.

## Validation rules (Rust)

A new `validate_subdir(subdir: &str) -> Result<String>` returns the canonical slash-joined form (which must equal the input):

- Reject empty (use omission instead).
- Reject any leading or trailing whitespace — the input must be exactly canonical, not "canonicalizable", so `locked.subdir == spec.subdir` is a sound string comparison without re-running the validator.
- Reject backslashes in the raw input (do not rely on `Path::components()`, which treats `\` as a separator on Windows). Splitting is done on `'/'` explicitly.
- Reject leading or trailing `'/'` and empty components (`a//b`).
- Reject `.` or `..` components, control characters, and glob meta (`*?[]!#` plus whitespace) — the same rules `validate_path_component` applies to `path`.
- Reject any component starting with `-` (defence in depth against argument-injection into `git sparse-checkout set`; the call also passes `--` to terminate option parsing).
- Allow either `vendor` value. With `vendor: false`, the selected subtree is generated content and cuenv adds `path` to `.gitignore`.

At evaluation time (`sync_vcs_dependencies`), a `subdir` failing any rule produces a configuration error before any git operations run.

## Implementation outline

`crates/cuenv/src/commands/sync/providers/vcs.rs`:

1. **Spec & lock types**

   - `cuenv_core::manifest::VcsDependency` gains `pub subdir: Option<String>`.
   - `cuenv_core::lockfile::LockedVcsDependency` gains:
     - `pub subdir: Option<String>` — copy of the spec value.
   - `pub subtree: Option<String>` — git tree hash of `<commit>:<subdir>`, populated only when `subdir` is set.
     - Both use `#[serde(default, skip_serializing_if = "Option::is_none")]` so existing lockfiles continue to deserialise. Lockfile version stays at the current value (additive change).
   - `locked_matches` extends to compare `subdir` alongside the existing fields.

2. **`resolve_dependency`** — when `subdir` is set:

   - After existing `clone`/`fetch`, verify the subdir resolves to a tree object and capture its hash:
     ```
     git rev-parse FETCH_HEAD^{commit}    -> commit
     git cat-file -t FETCH_HEAD:<subdir>  -> must equal "tree"
     git rev-parse FETCH_HEAD:<subdir>    -> subtree
     ```
     A non-zero exit on `cat-file` (subdir missing at ref) or a value other than `"tree"` (subdir points at a file) surfaces a clear configuration error before any installation begins.
   - Root `tree` field continues to record `FETCH_HEAD^{tree}` for backward compatibility, but is not consulted when `subdir` is set.

3. **`prepare_dependency`** — when `subdir` is set:

   - Validate inputs as today.
   - Sparse clone the dependency into a temp path:
     ```
     git clone --filter=blob:none --no-checkout <url> <tmp>
     git -C <tmp> sparse-checkout init --cone
     git -C <tmp> sparse-checkout set -- <subdir>
     git -C <tmp> fetch origin <commit>
     git -C <tmp> checkout --detach <commit>
     ```
   - Verify the subtree exists and matches:
     ```
     git -C <tmp> rev-parse HEAD:<subdir> == locked.subtree
     ```
   - Move `<tmp>/<subdir>` to the install target's temporary location (the existing `temp_target` flow). Discard the rest of `<tmp>`, including `.git`, before installation.
   - Continue with `ensure_dependency_does_not_reserve_marker` and `write_ownership_marker` against the subtree directory.

4. **`verify_checked_out_tree` / `check_materialized`** — when locked has `subdir`:

   - Path-on-disk verification uses `vendored_tree_hash(target)` against `locked.subtree` instead of `locked.tree`.
   - `vendored_tree_hash_with_git_dir` is unchanged; it just hashes whatever directory it is pointed at.

5. **Lockfile reuse** — re-syncing with the same spec hits the existing fast path (`locked_matches` returns true → no git operations). `-u`/`--update` clones and re-resolves; sparse path applies whenever `subdir` is set on the spec.

## Edge cases

- **`subdir` changes for the same `path`** — `locked_matches` returns false, so we re-resolve. `ensure_replaceable_target` already requires the marker to match the previously-locked commit, so the dirty/overwrite guards still apply. A successful re-sync overwrites with the new subtree.
- **`subdir` toggled from set to unset (or vice versa)** — handled identically by `locked_matches` rejection; resolved entry replaces the lock entry. Tree-hash verification picks the right field based on whether `locked.subdir.is_some()`.
- **Two deps with same `url`+`reference` but different `subdir`** — independent entries with distinct `path` values. The existing overlapping-paths detection prevents conflicting install paths. No special-casing required.
- **`subdir` pointing at a file rather than a directory** — `git sparse-checkout set` succeeds, but the moved target would be a single file rather than a directory. Reject this early in `resolve_dependency` by checking that the tree object at `<commit>:<subdir>` is a tree (`git cat-file -t <commit>:<subdir> == "tree"`).
- **`subdir` not in cone-mode-compatible form** — only enforced by the validator's rejection of glob characters. Cone mode requires literal directory names; the existing component validator already enforces this.

## Conflict & tampering safety

- The `.cuenv-vcs` marker stays at the install root and continues to hold the commit SHA. No format change.
- `ensure_replaceable_target` is unchanged; vendored-tree-hash verification uses `subtree` when present, otherwise `tree`.
- `sync_gitignore` continues to use the install `path`; for non-vendored subdir dependencies the whole materialized subtree is ignored.
- `prune_removed_vcs_dependencies` is unaffected: pruning operates on the install path, not on which subtree was vendored.

## Documentation updates

- `docs/design/specs/schema-coverage-matrix.md` — update the VCS row to note `subdir` as `implemented` once the change ships.
- `.agents/skills/cuenv-tools-lock-vcs/SKILL.md` — add `subdir` to the dependency surface, with a one-line note that `vendor: false` materializes ignored generated content.
- New how-to: `docs/guides/syncing-agent-skills.md` — show two recipes:
  1. Fetch cuenv's own bundled skills at a pinned release tag.
  2. Fetch a third-party skill pack from its repo.
- `cuenv task ci.schema-docs-check` must pass post-change.

## Testing

Unit tests in `vcs.rs`, using a local source repo with a known multi-directory tree (extend `create_source_repo` to add a few directories with content):

1. `subdir` extracts only the requested subdirectory; sibling content is absent at `path`.
2. `subdir` with `vendor: false` materializes the subtree, writes no nested `.git`, and ignores the target path.
3. Invalid `subdir` values rejected: empty, `..`, leading `/`, glob characters, control characters.
4. `subdir` referencing a non-existent path at the pinned ref produces a clear "subdir not present at reference" error.
5. `subdir` referencing a blob (file) rather than a tree is rejected at resolve time.
6. Lockfile round-trip: `subdir` and `subtree` persist; an entry without them still loads.
7. Re-sync without `-u` reuses the locked commit and subdir — no git network traffic.
8. `check` mode rejects modification of vendored subtree content (mirrors the existing `check_rejects_modified_vendored_content` test).
9. Changing `subdir` while keeping `path` unchanged re-materialises correctly (existing dirty/overwrite guards remain in force).
10. Two dependencies sharing `url`+`reference` but with different `subdir` and different `path` resolve independently.

No BDD tests planned for v1 — the unit coverage already exercises real git via the existing test harness.
The Nix build source filter must keep `.agents/skills/**` in the test source,
because the end-to-end sparse-subdir test seeds a local Git repository from
the checked-out skill files before running `cuenv sync vcs`.

## Risks & open questions

- **Network behaviour for `--filter=blob:none`** — requires a server that supports partial clone (most modern hosts do, including GitHub). If a server does not, the clone falls back gracefully but downloads all blobs. We treat that as acceptable; the subdir filter still scopes the materialised tree.
- **Sparse-checkout cone mode availability** — requires git ≥ 2.27 (already a reasonable floor for the project). Document in the matrix entry.
- **Lockfile compatibility with older cuenv binaries** — additive fields with `serde(default)` mean older binaries ignore them, but they would also silently re-vendor the whole repo if the spec carries `subdir` and the binary doesn't understand it. Not a v1 concern; flagging for the next minor-version release notes.
