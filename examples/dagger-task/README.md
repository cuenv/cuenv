# Dagger Task Example

This example demonstrates how to run tasks using the Dagger backend in `cuenv`.

## Prerequisites

1.  **Dagger Engine**: Ensure the Dagger engine is installed and running.
2.  **Feature Flag**: `cuenv` must be built with the `dagger-backend` feature (enabled by default in this branch).

## Usage

To run these tasks using the Dagger backend, use the `--backend dagger` flag.

### 1. Hello World (Alpine)

```bash
cuenv task hello --backend dagger
```

Output:
```text
Hello from a Dagger container!
```

### 2. Python Environment

Runs a Python snippet inside a `python:3.11-slim` container.

```bash
cuenv task python-info --backend dagger
```

Output:
```text
Running Python 3.11.x ... in Dagger
```

### 3. Environment Variables

Demonstrates passing environment variables into the container.

```bash
cuenv task env-check --backend dagger
```

Output:
```text
The secret is: visible-in-container
```
