# Dagger Task Example

This example demonstrates how to run tasks using the Dagger backend in `cuenv`.

## Prerequisites

1.  **Dagger Engine**: Ensure the Dagger engine is installed and running.
2.  **Feature Flag**: `cuenv` must be built with the `dagger-backend` feature (enabled by default in this branch).

## Configuration

The `env.cue` in this directory is configured to use `dagger` by default:

```cue
config: {
	backend: {
		type: "dagger"
		options: {
			image: "alpine:latest"
		}
	}
}
```

## Usage

Run tasks as normal. They will execute in Dagger containers because of the config above.

### 1. Hello World (Alpine)

Prints the OS release info to verify it's running in Alpine Linux (not your host).

```bash
cuenv task hello
```

Output:

```text
Hello from Dagger!
Container OS:
NAME="Alpine Linux"
ID=alpine
```

### 2. Python Environment

Runs a Python snippet inside a `python:3.11-slim` container.

```bash
cuenv task python-info
```

### 3. Environment Variables

Demonstrates passing environment variables into the container.

```bash
cuenv task env-check
```

### Overriding the Backend

You can force execution back to the host using the CLI flag:

```bash
cuenv task hello --backend host
```

(This might fail if the host doesn't have the tools or paths expected by the container task, but useful for debugging).
