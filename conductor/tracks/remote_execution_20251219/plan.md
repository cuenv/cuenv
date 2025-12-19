# Implementation Plan - Bazel Remote Execution API (REAPI) Client Support

This plan outlines the steps to implement the `cuenv-remote` crate, enabling `cuenv` to use remote execution servers via the REAPI v2 protocol.

## Phase 1: Foundation
Set up the crate structure, vendor protos, and implement core data structures.

- [x] Task: Create `crates/remote/` crate structure and `Cargo.toml` <!-- b53b199 -->
- [x] Task: Vendor REAPI protos and configure `tonic-build` <!-- 9170999 -->
- [x] Task: Implement `Digest` newtype and Merkle tree builder <!-- 5cab7b1 -->
    - [ ] Sub-task: Write unit tests for `Digest` and Merkle tree logic
    - [ ] Sub-task: Implement `Digest` and `Directory` tree construction from `ResolvedInputs`
- [x] Task: Conductor - User Manual Verification 'Phase 1: Foundation' (Protocol in workflow.md) [checkpoint: 2f5bd85]

## Phase 2: gRPC Clients
Implement the core gRPC service clients with retry logic.

- [x] Task: Implement `CasClient` for Content-Addressable Storage <!-- abdedee -->
    - [ ] Sub-task: Write unit tests for CAS operations (finding missing blobs, batch upload/read)
    - [ ] Sub-task: Implement `CasClient` logic using `tonic`
- [x] Task: Implement `ActionCacheClient` <!-- cde7a7a -->
    - [ ] Sub-task: Write unit tests for ActionCache (get/update action result)
    - [ ] Sub-task: Implement `ActionCacheClient` logic
- [x] Task: Implement `ExecutionClient` and `CapabilitiesClient` <!-- e69a1c2 -->
    - [ ] Sub-task: Write tests for streaming execution operations
    - [ ] Sub-task: Implement `ExecutionClient` with streaming progress support
- [x] Task: Implement gRPC retry logic with exponential backoff <!-- dd6c280 -->
    - [ ] Sub-task: Write tests for retry scenarios
    - [ ] Sub-task: Implement retry middleware or wrapper
- [x] Task: Conductor - User Manual Verification 'Phase 2: gRPC Clients' (Protocol in workflow.md) [checkpoint: 651641a]

## Phase 3: Task Mapping
Translate `cuenv` internal types to REAPI gRPC messages.

- [x] Task: Implement `CommandMapper` (cuenv Task → REAPI Command) <!-- 8531ca2 -->
    - [ ] Sub-task: Write tests for mapping complex tasks and scripts
    - [ ] Sub-task: Implement mapping logic, ensuring local secret resolution
- [x] Task: Implement `ActionBuilder` <!-- dfbf9db -->
    - [ ] Sub-task: Write tests for constructing the full REAPI Action message
    - [ ] Sub-task: Implement `ActionBuilder`
- [x] Task: Conductor - User Manual Verification 'Phase 3: Task Mapping' (Protocol in workflow.md) [checkpoint: 60d2b26]

## Phase 4: RemoteBackend Implementation
Implement the `TaskBackend` trait to glue everything together.

- [x] Task: Implement the `RemoteBackend` struct and `TaskBackend` trait <!-- b1342b5 -->
    - [ ] Sub-task: Write integration tests using a mock REAPI server if possible
    - [ ] Sub-task: Implement the full execution flow: secret resolution -> Merkle build -> cache check -> upload -> execute -> download
- [ ] Task: Conductor - User Manual Verification 'Phase 4: RemoteBackend Implementation' (Protocol in workflow.md)

## Phase 5: Core Integration
Expose the remote backend through the main `cuenv` CLI and configuration.

- [ ] Task: Update `BackendConfig` and CUE schema in `cuenv-core`
- [ ] Task: Update CLI and factory functions to support `--backend remote`
- [ ] Task: Conductor - User Manual Verification 'Phase 5: Core Integration' (Protocol in workflow.md)

## Phase 6: Polish and Documentation
Final touches and user-facing documentation.

- [ ] Task: Add comprehensive documentation for remote backend configuration
- [ ] Task: Final verification and cleanup
- [ ] Task: Conductor - User Manual Verification 'Phase 6: Polish and Documentation' (Protocol in workflow.md)
