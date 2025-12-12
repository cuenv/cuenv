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
- **Caching**: content-aware caching with materialization and sharing strategies.
- **Monorepos**: better workspace detection, per‑workspace locks, and ergonomics.

## Later

- **Remote caching**: share artifacts between CI and developer machines.
- **IDE experience**: smooth authoring and validation feedback loops.
