# Phase 1 — FFI safety, error contract, licensing, and limits

Status: Planned
Owners: Core
Target window: 1–2 weeks

## Goals

- Stabilize the Rust↔Go FFI contract with a strict JSON envelope and version handshake.
- Enforce input/output limits; eliminate undefined behavior risk around FFI strings.
- Align licensing (Cargo.toml vs README) before distribution.
- Introduce newtypes and explicit error handling (no `?`) to lock invariants at compile-time.

## Scope (must)

- Licensing alignment:
  - Pick a single license (MIT/Apache-2.0 dual OR AGPL-3.0-or-later). Update:
    - [ ] LICENSE
    - [ ] README badges and copy
    - [ ] [workspace.package] license in `Cargo.toml`
    - [ ] crates/\* Cargo.toml metadata
- Go bridge error envelope and version:
  - Go always returns a JSON envelope (never "error:" prefix), preserving field order for "ok".
  - Export `cue_bridge_version()` (C string) and embed a static ABI `BRIDGE_ABI="bridge/1"`.
  - Error schema:
    ```json
    {
      "version": "bridge/1",
      "ok": null,
      "error": { "code": "LOAD_INSTANCE", "message": "...", "hint": null }
    }
    ```
  - Success schema:
    ```json
    {"version":"bridge/1","ok":{...ordered json...},"error":null}
    ```
- Rust FFI caller updates:
  - Replace string prefix checks with strict `serde_json` parsing of the envelope.
  - Enforce `cuenv_core::Limits` on inputs/outputs.
  - Introduce newtypes:
    - `PackageDir` (absolute path, UTF‑8, length ≤ max_path_length)
    - `PackageName` (ASCII/[_a-zA-Z0-9], length ≤ max_package_name_length)
  - `CStringPtr` is explicitly non-Send/Sync (`impl !Send for ...` and `impl !Sync for ...`).
  - Replace `?` with explicit `match`/`if let` and map to `cuenv_core::Error`.
- Build gating:
  - Feature `go-bridge` to build/link the Go archive; provide a stub evaluator when disabled or Go not present (returns `Error::Ffi` with help).
- Tests:
  - Envelope success/error roundtrip tests.
  - Limits enforcement tests.
  - Compile-fail or trait tests to ensure `CStringPtr` is `!Send + !Sync`.
  - Deterministic order test (fields are in CUE order).
- CI (incremental):
  - Pin Go to 1.24.x (same everywhere). Fail fast on mismatch.
  - Keep Windows Go tests; add macOS build target in a later phase.

## Out of scope (defer)

- OTel integration
- Benchmarks
- Security sandboxing

## Design details

### Go changes (crates/cuengine/bridge.go)

- Add:
  - `//export cue_bridge_version`
  - Return `"bridge/1"` concatenated with Go version (for diagnostics).
- Normalize return shape:
  - On success: `{"version":"bridge/1","ok":<ordered-json>,"error":null}`
  - On failure: `{"version":"bridge/1","ok":null,"error":{"code": "...", "message": "...", "hint": null}}`
- Error codes: `INVALID_INPUT`, `LOAD_INSTANCE`, `BUILD_VALUE`, `ORDERED_JSON`, `PANIC_RECOVER`
- Preserve ordered JSON using existing `buildOrderedJSONString`.

### Rust changes (crates/cuengine/src/lib.rs)

- Add envelope type:

  ```rust
  #[derive(serde::Deserialize)]
  struct BridgeError { code: String, message: String, hint: Option<String> }

  #[derive(serde::Deserialize)]
  struct BridgeEnvelope<'a> {
      version: String,
      #[serde(borrow)]
      ok: Option<&'a serde_json::value::RawValue>,
      error: Option<BridgeError>,
  }
  ```

- Convert pointer to &str, parse envelope, map `error` to `Error::cue_parse` or `Error::ffi`.
- Enforce limits (use `Limits::default()` unless overridden).
- Negative impls:
  ```rust
  impl !Send for CStringPtr {}
  impl !Sync for CStringPtr {}
  ```
- Replace `?` with explicit matches and trace errors with context.

### Newtypes (crates/cuenv-core or cuengine)

- `PackageDir(String)` and `PackageName(String)` with `TryFrom` validation.
- Prohibit interior NUL, non-UTF‑8, and length violations at construction.

### build.rs (crates/cuengine/build.rs)

- Respect Cargo feature `go-bridge`. If disabled or `go` missing:
  - Skip building the archive.
  - Compile a cfg gate so the Rust side uses a stub that returns `Error::ffi_with_help(...)`.
- Surface helpful help text: "Enable feature go-bridge or ensure Go 1.24+ installed".

## Acceptance criteria

- FFI returns validated envelope; no "error:" prefix remains.
- Unit/integration tests cover both success/error. 100% of new code paths tested.
- `CStringPtr` is not Send/Sync; MIRI basic pass on unix (added in Phase 3).
- Limits enforced with tests.
- License aligned across workspace and docs.

## Commands (run from repo root; devenv required)

- devenv shell -- cargo fmt --all
- devenv shell -- cargo clippy --workspace --all-targets --all-features -- -D warnings
- devenv shell -- cargo nextest run --workspace
- devenv shell -- cargo test --doc --workspace

## Risks

- Envelope change may require downstream updates later; we lock "bridge/1" early to minimize churn.

## Backout

- Keep old parsing shim for one release (optional), behind a feature, then remove.
