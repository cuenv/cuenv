# Implementation Plan - Bazel Remote Execution API (REAPI) Client Support

This plan outlines the steps to implement the `cuenv-remote` crate, enabling `cuenv` to use remote execution servers via the REAPI v2 protocol.

## Phase 1: Foundation
Set up the crate structure, vendor protos, and implement core data structures.

- [x] Task: Create `crates/remote/` crate structure and `Cargo.toml` <!-- b53b199 -->
- [ ] Task: Vendor REAPI protos and configure `tonic-build`
- [ ] Task: Implement `Digest` newtype and Merkle tree builder
    - [ ] Sub-task: Write unit tests for `Digest` and Merkle tree logic
    - [ ] Sub-task: Implement `Digest` and `Directory` tree construction from `ResolvedInputs`
- [ ] Task: Conductor - User Manual Verification 'Phase 1: Foundation' (Protocol in workflow.md)

## Phase 2: gRPC Clients
Implement the core gRPC service clients with retry logic.

- [ ] Task: Implement `CasClient` for Content-Addressable Storage
    - [ ] Sub-task: Write unit tests for CAS operations (finding missing blobs, batch upload/read)
    - [ ] Sub-task: Implement `CasClient` logic using `tonic`
- [ ] Task: Implement `ActionCacheClient`
    - [ ] Sub-task: Write unit tests for ActionCache (get/update action result)
    - [ ] Sub-task: Implement `ActionCacheClient` logic
- [ ] Task: Implement `ExecutionClient` and `CapabilitiesClient`
    - [ ] Sub-task: Write tests for streaming execution operations
    - [ ] Sub-task: Implement `ExecutionClient` with streaming progress support
- [ ] Task: Implement gRPC retry logic with exponential backoff
    - [ ] Sub-task: Write tests for retry scenarios
    - [ ] Sub-task: Implement retry middleware or wrapper
- [ ] Task: Conductor - User Manual Verification 'Phase 2: gRPC Clients' (Protocol in workflow.md)

## Phase 3: Task Mapping
Translate `cuenv` internal types to REAPI gRPC messages.

- [ ] Task: Implement `CommandMapper` (cuenv Task → REAPI Command)
    - [ ] Sub-task: Write tests for mapping complex tasks and scripts
    - [ ] Sub-task: Implement mapping logic, ensuring local secret resolution
- [ ] Task: Implement `ActionBuilder`
    - [ ] Sub-task: Write tests for constructing the full REAPI Action message
    - [ ] Sub-task: Implement `ActionBuilder`
- [ ] Task: Conductor - User Manual Verification 'Phase 3: Task Mapping' (Protocol in workflow.md)

## Phase 4: RemoteBackend Implementation
Implement the `TaskBackend` trait to glue everything together.

- [ ] Task: Implement the `RemoteBackend` struct and `TaskBackend` trait
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
