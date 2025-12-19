# Product Guidelines - cuenv

## Design Philosophy
*   **Secure by Default:** Secrets must never touch the disk in plaintext. The system should prioritize security over convenience if a conflict arises.
*   **Validation First:** All environment variables and task configurations must be strictly typed and validated using CUE before execution.
*   **Performance Focused:** Task execution should maximize parallelism and leverage intelligent caching to minimize developer wait times.
*   **Platform Agnostic:** The core engine should remain independent of specific CI/CD vendors, facilitating local execution and remote build farm compatibility.

## Documentation Standards
*   **Precision:** Use technical and precise language. Documentation should be authoritative and accurate.
*   **Clarity:** Use clear examples for every CUE schema and CLI command.
*   **Contextual:** Explain *why* a design decision was made, especially regarding security and performance trade-offs.

## Visual and User Interface (TUI)
*   **Functional Aesthetic:** The TUI should prioritize information density and clarity. Use color sparingly but effectively to indicate status (e.g., success, failure, in-progress).
*   **Responsive:** The CLI and TUI must remain responsive even during heavy task execution.
*   **Redaction:** Ensure sensitive information is visually redacted in all UI outputs.
