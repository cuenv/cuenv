---
title: Roadmap
description: Where cuenv is going next.
---

This roadmap focuses on making cuenv excellent for day‑to‑day development and CI: fast environment loading, reliable task execution, and first‑class secret handling.

## Now

- **Shell integration**: fast, predictable, safe directory entry/exit lifecycle.
- **Secrets**: runtime resolution, redaction, and policy enforcement across exec + tasks.
- **Tasks UX**: clear progress for parallel execution and dependency graphs.

## Next

- **Hermetic execution**: run tasks with declared inputs/outputs only (sandboxing).
- **Caching**: host tasks keep the local content-addressed cache; containerized work uses Dagger's engine cache ([RFC-0007](/decisions/rfcs/rfc-0007-dagger-v1-native-runtime/)).
- **Dagger v1 native runtime**: `#DaggerRuntime` executes as written, one session per graph, Dockerfile images through Dagger. See [RFC-0007](/decisions/rfcs/rfc-0007-dagger-v1-native-runtime/).
- **Monorepos**: better workspace detection, per‑workspace locks, and ergonomics.

## Later

- **Remote caching**: share *host-task* artifacts between CI and developer machines. Container reuse is Dagger's job.
- **IDE experience**: smooth authoring and validation feedback loops.
