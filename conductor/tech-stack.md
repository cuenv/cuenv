# Tech Stack - cuenv

## Core Technologies
*   **Programming Languages:**
    *   **Rust:** Primary language for CLI, TUI, task orchestration, and core logic.
    *   **Go:** Used for the CUE evaluation engine, integrated via a C FFI bridge.
*   **Build Systems & Dependency Management:**
    *   **Cargo:** Rust package manager and build tool.
    *   **Nix (Flakes):** For hermetic development environments and reproducible builds.
    *   **Bun:** Package manager for documentation and frontend integrations.
*   **Frameworks & Libraries:**
    *   **Tokio:** Asynchronous runtime for Rust.
    *   **Clap:** CLI argument parsing.
    *   **Ratatui:** For building the Terminal User Interface (TUI).
    *   **Dagger:** Task execution backend for hermetic and containerized tasks.
    *   **Astro:** Documentation framework.

## Architecture
*   **Monorepo:** Modular Rust workspace structure in `crates/`.
*   **FFI Bridge:** Go-to-Rust interface for high-performance CUE evaluation.
*   **Event-Driven:** Internal event system (`crates/events`) for UI and execution updates.

## Development Environment
*   **VCS:** Git.
*   **CI/CD:** GitHub Actions (Current), moving towards generic Remote Build Farm support.
