# cuenv Roadmap

This roadmap tracks production readiness work. Each phase has clear entry/exit criteria.

- **Phase 1** — FFI safety, error contract, licensing, and limits
  - Status: Planned
  - File: PHASE-1.md
  - Exit: Envelope + limits + license aligned + tests green

- **Phase 2** — CLI UX, API hygiene, docs, and tracing
  - Status: Planned
  - File: PHASE-2.md
  - Exit: CLI exit codes, JSON stability, docs complete, clippy pedantic clean

- **Phase 3** — Build, CI, packaging, and supply chain
  - Status: Planned
  - File: PHASE-3.md
  - Exit: 3-OS CI green, reproducible builds, SBOM and deny checks passing

- **Phase 4** — Performance and memory
  - Status: Planned
  - File: PHASE-4.md
  - Exit: Baselines recorded, no leaks, budgets documented

- **Phase 5** — Security hardening and diagnostics
  - Status: Planned
  - File: PHASE-5.md
  - Exit: Sanitizers green, capability POC merged

## Contributing

- Work items and acceptance criteria live in each PHASE-\*.md.
- Use `devenv shell --` prefix for all commands within the repo environment.
- Each phase builds on the previous one - complete Phase 1 before moving to Phase 2.

## Timeline

- **Weeks 1-2**: Phase 1 (critical ship blockers)
- **Week 3**: Phase 2 (polish and usability)
- **Weeks 4-5**: Phase 3 (infrastructure and packaging)
- **Week 6**: Phase 4 (performance validation)
- **Weeks 7-8+**: Phase 5 (security hardening)
