---
name: cuenv-tools-lock-vcs
description: Use for cuenv tools runtime, tool sources, platform overrides, extraction behavior, activation, lock sync, and VCS dependencies. Covers schema/tools.cue and schema/vcs.cue.
---

# Tools, Lock, VCS

Read `docs/design/specs/schema-coverage-matrix.md`, then inspect:

- `schema/tools.cue` for `#ToolsRuntime`, `#Tool`, overrides, source unions, extracts, and activation.
- `schema/vcs.cue` for VCS dependency definitions.
- `crates/core/src/tools/activation.rs` for activation schema/mutation rules and `crates/core/src/tools/activation/path_index.rs` for lockfile provider path discovery.
- `crates/cuenv/src/commands/tools.rs` and sync providers when behavior matters.
- `crates/cuenv/src/commands/sync/providers/vcs.rs` for VCS sync orchestration, `crates/cuenv/src/commands/sync/providers/vcs/materialization.rs` for checkout and marker verification, `crates/cuenv/src/commands/sync/providers/vcs/paths.rs` for path/temp safety, and `crates/cuenv/src/commands/sync/providers/vcs/git.rs` for git subprocess execution.
- `crates/ci/src/executor/tools.rs` for CI task tool downloads and lockfile activation.
- `crates/tools/github/src/lib.rs` for GitHub release API resolution, asset selection, downloads, and cache target planning; `crates/tools/github/src/extract.rs` for archive extraction behavior.
- `crates/tools/url/src/lib.rs` for URL resolution/cache placement and `crates/tools/url/src/extract.rs` for URL archive extraction behavior.
- `crates/tools/oci/src/provider.rs` for registry/manifest/cache orchestration and `crates/tools/oci/src/extract.rs` for single-binary extraction from image layers.

Status guardrails:

- Nix, GitHub, Rustup, URL, and OCI tool providers are all registered.
- The OCI provider (`crates/tools/oci`) extracts a single binary at `path` from the platform-specific manifest layers; `image` and `path` both honor `{version}`, `{os}`, and `{arch}` templates. Multi-arch image indexes are walked and the matching `os`/`architecture` entry is selected.
- Tool lock source-template replacement in `crates/cuenv/src/commands/sync/providers/lock/tool_resolution.rs` owns the literal `{version}`, `{os}`, and `{arch}` placeholders as constants; keep it warning-free without formatting-lint suppressions.
- GitHub and URL providers auto-extract `.zip`, `.tar.gz`/`.tgz`, and `.tar.xz`/`.txz`. GitHub additionally extracts `.pkg` on macOS. Unknown extensions are treated as raw binaries — point users at a supported archive form rather than letting compressed bytes get written to disk.
- GitHub tool-provider token precedence tests in `crates/tools/github/src/tests.rs` should use scoped `temp_env` overrides for `GITHUB_TOKEN` / `GH_TOKEN`, not unsafe process-wide environment mutation.
- Use `cuenv sync vcs` for VCS dependencies.
- Use `cuenv tools activate` for lockfile activation metadata.
- `#VcsDependency.subdir` performs sparse-checkout of a single subtree. The lockfile records the subtree hash and re-syncs are deterministic; `vendor: false` ignores the materialized subtree instead of leaving a nested `.git` checkout.

Adversarial prompts:

- "Install a tool from an OCI image." Use `source: #Oci & {image: "...", path: "/usr/bin/tool"}`. Confirm the image is multi-arch with entries for every platform in `platforms`; otherwise pair with `#Override` for platform-specific sources.
- "Add platform-specific GitHub release assets." Use `#Override` and `#GitHubExtract`.
- "Sync VCS dependencies." Use `schema.#VcsDependency` and `cuenv sync vcs`.
- "Materialize only one directory of a remote repo (e.g. agent skills)." Use `subdir`; cuenv runs a sparse-checkout and lands only that subtree at `path`.
