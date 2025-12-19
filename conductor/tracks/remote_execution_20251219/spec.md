# Specification: Bazel Remote Execution API (REAPI) Client Support

## Overview
This track involves adding a new `cuenv-remote` crate to implement a client for the Bazel Remote Execution API v2 (REAPI). This will allow `cuenv` to distribute task execution across remote workers using any REAPI-compatible server (e.g., BuildBarn, Buildfarm, BuildBuddy, EngFlow).

## Functional Requirements
- **Remote Task Execution:** Distribute `cuenv task` execution to REAPI-compatible servers.
- **Content-Addressable Storage (CAS):** Implement a client for the REAPI CAS to upload task inputs and download outputs using Merkle trees.
- **Action Cache Integration:** Check the REAPI Action Cache for existing results before executing tasks remotely.
- **Secret Resolution:** All secrets must be resolved locally by the coordinator (`cuenv` CLI) before sending tasks to remote workers. No secret references or resolver commands should be sent to workers.
- **Progress Streaming:** Stream execution progress from the remote server back to `cuenv` events for real-time display in the TUI/CLI.
- **Global Backend Selection:** Support a global `--backend remote` CLI flag to override the default local execution for all tasks in a session.
- **Bearer Token Authentication:** Implement initial support for token-based authentication (Bearer tokens).

## Non-Functional Requirements
- **Protocol:** Use gRPC (via the `tonic` crate) for all communications with the REAPI server.
- **Efficiency:** Use binary protocols and content-addressed deduplication (Merkle trees) for efficient input/output handling.
- **Error Handling:** Provide detailed diagnostics using `miette` for remote execution failures.
- **Resiliency:** Implement exponential backoff and retry logic for gRPC calls.

## Acceptance Criteria
- [ ] A new `cuenv-remote` crate is created and integrated into the `cuenv` workspace.
- [ ] REAPI protos are vendored and compiled using `tonic-build`.
- [ ] `cuenv task <task> --backend remote` successfully executes a task on a remote REAPI server.
- [ ] Cache hits in the REAPI Action Cache correctly skip remote execution and download outputs locally.
- [ ] Parallel tasks in `cuenv` are executed concurrently on remote workers.
- [ ] Secrets are correctly resolved locally and passed as environment variables to the remote `Command`.
- [ ] Bearer token authentication works as configured in `env.cue`.

## Out of Scope
- **Server Implementation:** `cuenv` will only act as a client; users must provide their own REAPI-compatible server.
- **mTLS/Google Cloud Auth:** These are deferred to a later "Polish" phase.
- **Per-Task Backend Configuration:** Initially, the backend selection will be a global session-level decision.
