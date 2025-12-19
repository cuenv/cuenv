# Product Guide - cuenv

## Initial Concept
cuenv is a configuration utility and validation engine designed to simplify and secure development environments. It uses CUE to provide type-safe environment variables, secure secret resolution, and parallel task execution with content-aware caching.

## Target Users
*   **DevOps and Platform Engineers:** Managing complex environments, multiple stages, and sensitive secrets across teams.
*   **Software Developers:** Seeking a consistent, "it works on my machine" local development experience with zero-config shell integration.
*   **CI/CD Engineers:** Building faster, more reliable pipelines by leveraging parallel execution and intelligent caching.
*   **Type-safety Enthusiasts:** Developers who want the power of CUE to validate and constrain their configuration before runtime.

## Core Value Propositions
*   **`cuenv exec`:** Run any command within a validated, secure environment where secrets are resolved at runtime and never touch the disk.
*   **`cuenv task`:** Orchestrate complex workflows with automatic dependency resolution, parallel execution, and skipping work using content-aware caching.

## Strategic Differentiation
*   **Built-in Type Safety:** Unlike standard task runners, cuenv uses CUE for deep validation of environment variables, task inputs, and configuration patterns.
*   **Native Secret Orchestration:** Direct, first-class integration with 1Password, AWS, GCP, and Vault, ensuring secrets are redacted from logs and kept out of Git.
*   **CI/CD Independence:** The ability to run and generate optimized workflows locally and across various platforms, reducing reliance on vendor-specific features like GitHub Actions.
*   **Parallelism & Intelligent Caching:** Performance-focused execution that understands the dependency graph and only executes what is necessary.

## Current Focus & Roadmap
The current development phase is focused on providing a viable alternative to heavy platform-specific CI tools (like GitHub Actions) by ensuring that `cuenv` can leverage the benefits of remote build infrastructures (similar to Bazel Remote Build Farms) without the associated complexity or proprietary lock-in.
