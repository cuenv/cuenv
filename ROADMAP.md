### **Review: Cuenv vs. The World**

| Feature | **Just** (Task Runner) | **direnv** (Env Manager) | **Bazel** (Build System) | **Cuenv** (The Hybrid) |
| :--- | :--- | :--- | :--- | :--- |
| **Configuration** | Simple Makefile-like syntax. | `.envrc` (bash scripts). | Starlark (Python-like). | **CUE (Type-safe, validated).** |
| **Environment** | Static strings only. | Dynamic, but untyped. | Strict/Hermetic. | **Typed, Validated, Runtime Secrets.** |
| **Execution** | Linear/Direct. | N/A. | Graph-based, Hermetic. | **Graph-based (DAG), Parallel, Cached.** |
| **Secret Mgmt** | N/A (relies on shell). | N/A (relies on shell). | N/A (external). | **Native Runtime Resolution (1Password, etc).** |
| **Safety** | Low (shell scripts). | Low (arbitrary bash). | High (Sandboxed). | **High (Type validation + Planned Sandbox).** |

**Verdict:**
-   **Strengths:** Cuenv solves the "loose string" problem of modern DevOps. By validating environment variables (e.g., `PORT` must be an int < 65536) and graph dependencies, it catches errors before execution.
-   **Weaknesses:** It currently disables "hermetic execution" (sandboxing), meaning it cannot yet guarantee reproducible builds like Bazel. It is heavier than `Just` for simple tasks.

---

### **Cuenv Roadmap**

This roadmap focuses on graduating Cuenv from "Alpha" to a production-grade tool that can replace the Just/direnv stack and encroach on Bazel's territory for medium-sized monorepos.

#### **Phase 1: The Developer Experience (Q4 2025)**
*Goal: Make Cuenv the best local development tool, replacing `just` and `direnv`.*

1.  **Polished Shell Integration (`cuenv shell`)**
    *   **Objective:** Seamless shell hooking that rivals `direnv`.
    *   **Tasks:**
        *   Finalize `zsh`, `bash`, and `fish` hooks (currently in development).
        *   Implement the "Approval Gate" (`cuenv allow`) to prevent arbitrary code execution from new repos.
        *   Ensure sub-millisecond latency on prompt hooks to avoid shell lag.

2.  **Secret Provider Plugins**
    *   **Objective:** Native support for major secret managers.
    *   **Tasks:**
        *   Implement trait-based providers for **1Password**, **AWS Secrets Manager**, **Vault**, and **Doppler**.
        *   Ensure secrets are *only* in memory during process execution and never leaked to disk/logs.

3.  **Rich TUI for Task Execution**
    *   **Objective:** Visual feedback for parallel tasks.
    *   **Tasks:**
        *   Replace standard stdout streaming with a localized TUI (like `docker-compose` or `buck2`).
        *   Visual DAG representation: show which tasks are waiting on dependencies.

#### **Phase 2: Hermeticity & Correctness (Q1 2026)**
*Goal: Enable "Bazel-lite" reliability for builds and tests.*

4.  **Re-enable Hermetic Execution**
    *   **Objective:** Guarantee that tasks only access declared inputs.
    *   **Tasks:**
        *   Remove the "temporarily disabled" flag in `task.rs`.
        *   Implement OS-level sandboxing (using `bubblewrap` on Linux or `sandbox-exec` on macOS) to deny filesystem access outside declared inputs.
        *   **Critical:** Ensure `Nix` environments are correctly projected into the sandbox.

5.  **Advanced Caching**
    *   **Objective:** Never run the same task twice.
    *   **Tasks:**
        *   Implement content-addressable storage (CAS) for task outputs.
        *   Support shared local cache (e.g., `~/.cache/cuenv`) to share build artifacts across branches.

#### **Phase 3: Scale & Ecosystem (Q2 2026)**
*Goal: Support monorepos and team collaboration.*

6.  **Remote Caching**
    *   **Objective:** Share build artifacts between team members and CI.
    *   **Tasks:**
        *   Implement an HTTP/gRPC backend for the cache (compatible with Bazel Remote Execution API or simple S3/GCS storage).

7.  **IDE Support (LSP)**
    *   **Objective:** First-class editing experience.
    *   **Tasks:**
        *   Leverage the official CUE Language Server Protocol (LSP) for editor integration.
        *   Ensure Cuenv project structures are compatible with standard CUE LSP discovery for autocomplete and validation.

8.  **Nix Flake Integration**
    *   **Objective:** fully hermetic toolchains.
    *   **Tasks:**
        *   Allow `cuenv` to automatically fetch tools defined in `flake.nix` if they are missing, effectively making `cuenv` the only binary a developer needs to install.