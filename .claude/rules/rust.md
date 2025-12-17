---
paths: "**/*.rs"
---

# Rust Code Standards

These standards apply to all `.rs` files. They prioritize **safety**, **expressiveness**, and **maintainability**.

## I. Trait-Driven Design

> "Structs are nouns, Traits are verbs."

### 1. Prefer `impl Trait` for Arguments

When a function does not need to return a specific generic type or relate multiple arguments, use `impl Trait`. It is cleaner and easier to read than verbose generic bounds.

- **Standard:** Use `impl Trait` for simple argument abstraction.
- **Avoid:** `Box<dyn Trait>` unless runtime polymorphism (type erasure) is strictly required.

```rust
// ✅ DO: Clean and flexible
fn save_data(data: impl Serialize) { ... }

// ❌ DON'T: Unnecessary generic verbosity for simple cases
fn save_data<T: Serialize>(data: T) { ... }

// ⚠️ ACCEPTABLE: When runtime dispatch is needed (different implementations at runtime)
fn get_provider() -> Box<dyn Provider> { ... }
```

### 2. Implement Standard Interop Traits

Your types should feel "native" to the ecosystem.

- **Must Implement:** `Debug` (for all public types), `Send` + `Sync` (unless thread-safety is explicitly unsafe).
- **Should Implement:** `Default` (instead of a custom `new()` with no args), `Display` (for user-facing output), `From/Into` (for infallible conversions).

### 3. The "Sealed Trait" Pattern

If you define a trait that is intended for internal use only (i.e., you do not want downstream users to implement it), seal it. This allows you to add methods to the trait later without breaking changes.

```rust
// Private module
mod private {
    pub trait Sealed {}
}

// Public trait dependent on private Sealed trait
pub trait MyPublicTrait: private::Sealed {
    fn method(&self);
}
```

---

## II. Functional Programming

Idiomatic Rust leans heavily on FP patterns for data transformation and error handling.

### 1. Internal Iteration over External Iteration

Prefer Iterators (`map`, `filter`, `fold`) over explicit `for` loops when transforming data. This clearly signals intent (transformation vs. side-effect) and often allows the compiler to optimize bounds checks better.

```rust
// ✅ DO: Declarative, lazy, and chainable
let squares: Vec<i32> = numbers.iter()
    .filter(|&&x| x % 2 == 0)
    .map(|&x| x * x)
    .collect();

// ❌ DON'T: Imperative mutation is harder to parallelize/reason about
let mut squares = Vec::new();
for x in numbers {
    if x % 2 == 0 {
        squares.push(x * x);
    }
}

// ⚠️ ACCEPTABLE: When mutating existing data or graph structures
for node in &mut graph.nodes {
    node.update_state();
}
```

### 2. Expressions over Statements

Rust is an expression-based language. Treat blocks as values rather than just containers for statements.

- **Standard:** Return values directly from `if`, `match`, and blocks rather than assigning to a mutable variable.

```rust
// ✅ DO: Variable is immutable, logic is atomic
let status = if is_online { "Connected" } else { "Offline" };

// ❌ DON'T: Requires mutability and separated declaration
let mut status;
if is_online {
    status = "Connected";
} else {
    status = "Offline";
}
```

### 3. Combinators for Control Flow

Avoid nested `match` statements for `Option` and `Result`. Use combinators to flatten the logic pipeline.

- **Common Combinators:** `map`, `and_then` (flat_map), `unwrap_or`, `unwrap_or_else`.

```rust
// ✅ DO: Linear pipeline
let user_id = find_user(name)
    .map(|user| user.id)
    .unwrap_or(0);

// ❌ DON'T: Deep nesting (The "Arrow of Code")
let user_id = match find_user(name) {
    Some(user) => user.id,
    None => 0,
};
```

---

## III. Industry Best Practices

### 1. The "Newtype" Pattern for Safety

Never use primitive types (`bool`, `i32`, `String`) for domain concepts. Wrap them in a tuple struct. This prevents logic errors like passing a `user_id` into a function expecting a `product_id`.

```rust
struct UserId(pub u32);
struct ProductId(pub u32);

// The compiler now prevents you from mixing these up
fn process_order(user: UserId, product: ProductId) { ... }
```

### 2. Error Handling Hygiene

- **Library Code:** Use `thiserror` to define structured, distinct error enums. Never panic.
- **Application Code:** Use `thiserror` with a custom `CliError` type that maps to exit codes.
- **Strict Rule:** No `unwrap()` or `expect()` in production code. Use `?` propagation or safe handling.

```rust
// ✅ CLI error pattern
#[derive(Error, Debug)]
pub enum CliError {
    #[error("Configuration error: {0}")]
    Config(String),
    #[error("Evaluation failed: {0}")]
    Eval(String),
}

const EXIT_OK: i32 = 0;
const EXIT_CLI: i32 = 2;
const EXIT_EVAL: i32 = 3;

fn exit_code(err: &CliError) -> i32 {
    match err {
        CliError::Config(_) => EXIT_CLI,
        CliError::Eval(_) => EXIT_EVAL,
    }
}
```

### 3. The Builder Pattern

For structs with many fields or complex configuration, implement the Builder pattern rather than a constructor with 10 arguments.

- **Standard:** Manual builders are preferred. Use `derive_builder` only if it significantly reduces boilerplate.
- **All builder methods must be `#[must_use]`** for method chaining safety.

```rust
impl ConfigBuilder {
    #[must_use]
    pub fn timeout(mut self, duration: Duration) -> Self {
        self.timeout = Some(duration);
        self
    }

    pub fn build(self) -> Result<Config> {
        // Validation
    }
}
```

### 4. Module Visibility

Keep fields private by default. Expose data only through methods (getters).

- **Standard:** `struct` fields should generally be private.
- **Exception:** "Data Transfer Objects" (DTOs) or simple state holders where logic is strictly separated.

---

## IV. Error Diagnostics with Miette

Use `miette::Diagnostic` alongside `thiserror::Error` for rich, user-friendly error output with source context.

```rust
#[derive(Error, Diagnostic, Debug)]
pub enum Error {
    #[error("Configuration error: {message}")]
    #[diagnostic(
        code(cuenv::config),
        help("Check your env.cue file for syntax errors")
    )]
    Configuration {
        message: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("error here")]
        span: SourceSpan,
    },
}
```

### Error Constructor Methods

Provide convenience constructors with `#[must_use]` to prevent accidental error creation:

```rust
impl Error {
    #[must_use]
    pub fn configuration(message: impl Into<String>) -> Self {
        Self::Configuration { message: message.into() }
    }

    #[must_use]
    pub fn ffi(function: &'static str, message: impl Into<String>) -> Self {
        Self::Ffi { function, message: message.into() }
    }
}
```

---

## V. Async Patterns

### Structured Concurrency with JoinSet

Use `tokio::task::JoinSet` for bounded parallelism and clean error collection:

```rust
let mut join_set = JoinSet::new();

for task in tasks {
    join_set.spawn(async move {
        executor.execute_task(task).await
    });
}

while let Some(result) = join_set.join_next().await {
    match result {
        Ok(Ok(output)) => handle_success(output),
        Ok(Err(e)) => handle_task_error(e),
        Err(e) => handle_join_error(e),
    }
}
```

### Dual Execution Paths

Commands that don't need async should offer sync alternatives to avoid runtime overhead:

```rust
pub fn run_sync(&self) -> Result<()> {
    // Synchronous implementation
}

pub async fn run_async(&self) -> Result<()> {
    // Async implementation with tokio
}
```

---

## VI. Tracing & Observability

### Instrument Functions

All significant public functions should use `#[tracing::instrument]`:

```rust
#[tracing::instrument(
    name = "evaluate_cue_package",
    fields(path = %path.display(), package = package_name),
    level = "info",
    skip(self)
)]
pub fn evaluate(&self, path: &Path, package_name: &str) -> Result<String> {
    tracing::info!("Starting evaluation");
    // ...
    tracing::debug!(bytes = result.len(), "Evaluation complete");
    Ok(result)
}
```

### Event-Driven Output

**Direct `println!`/`eprintln!` is forbidden** (enforced by clippy lint). All output must go through the event system:

```rust
// ❌ FORBIDDEN
println!("Task completed: {}", task_name);

// ✅ REQUIRED - Use event macros
emit_task_completed!(task_name, duration);
emit_task_failed!(task_name, error);
```

---

## VII. Newtype Validation with TryFrom

Newtypes should validate on construction using `TryFrom`:

```rust
#[derive(Debug, Clone, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct PackageName(String);

#[derive(Error, Diagnostic, Debug)]
pub enum PackageNameError {
    #[error("Package name cannot be empty")]
    Empty,
    #[error("Package name must start with alphanumeric character")]
    InvalidStart,
    #[error("Package name too long (max {max} chars, got {actual})")]
    TooLong { max: usize, actual: usize },
}

impl TryFrom<&str> for PackageName {
    type Error = PackageNameError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        if value.is_empty() {
            return Err(PackageNameError::Empty);
        }
        if !value.chars().next().unwrap().is_alphanumeric() {
            return Err(PackageNameError::InvalidStart);
        }
        if value.len() > 64 {
            return Err(PackageNameError::TooLong { max: 64, actual: value.len() });
        }
        Ok(Self(value.to_string()))
    }
}

impl AsRef<str> for PackageName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}
```

---

## VIII. Serde Conventions

### Standard Derive Pattern

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TaskConfig {
    pub command: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    #[serde(default)]
    pub depends_on: Vec<String>,

    #[serde(default = "default_timeout")]
    pub timeout_seconds: u64,
}

fn default_timeout() -> u64 {
    300
}
```

### Key Attributes

- `#[serde(rename_all = "camelCase")]` - JSON field names
- `#[serde(skip_serializing_if = "Option::is_none")]` - Omit None fields
- `#[serde(default)]` - Use Default::default() for missing fields
- `#[serde(default = "function")]` - Custom default value
- `#[serde(untagged)]` - Enum without type tag

---

## IX. FFI Safety (cuengine)

### RAII Wrappers for Foreign Memory

```rust
pub struct CStringPtr {
    ptr: *mut c_char,
    _marker: PhantomData<*const ()>,  // Makes !Send + !Sync
}

impl Drop for CStringPtr {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            // SAFETY: cue_free_string is the designated deallocator for
            // strings returned by the Go FFI bridge. The pointer was obtained
            // from a previous FFI call and has not been freed.
            unsafe { cue_free_string(self.ptr); }
        }
    }
}
```

### Safety Documentation

All `unsafe` blocks must have multi-line `// SAFETY:` comments:

```rust
// SAFETY: The Go FFI bridge guarantees that:
// 1. The returned pointer is valid UTF-8
// 2. The string is null-terminated
// 3. Memory is allocated by Go's allocator
// 4. cue_free_string is the correct deallocator
let result = unsafe { CStr::from_ptr(ptr) };
```

---

## X. Feature Flags

### Naming Convention

```toml
[features]
default = []                           # Empty by default (opt-in model)
github = ["dep:octocrab"]              # VCS providers
gitlab = ["dep:gitlab"]
dagger-backend = ["dep:cuenv-dagger"]  # Optional backends
```

- **Lowercase kebab-case** for all feature names
- **Empty default** - all features opt-in
- **Conditional compilation** with `#[cfg(feature = "...")]`

```rust
#[cfg(feature = "github")]
pub mod github;

#[cfg(feature = "dagger-backend")]
pub async fn execute_with_dagger(task: &Task) -> Result<Output> {
    // ...
}
```

---

## XI. Test Utilities

### Shared Test Helpers

Place reusable test helpers in a `test_utils` module:

```rust
#[cfg(test)]
pub mod test_utils {
    use super::*;

    pub fn create_task(name: &str, deps: Vec<&str>) -> Task {
        Task {
            command: format!("echo {}", name),
            depends_on: deps.iter().map(|s| s.to_string()).collect(),
            description: Some(format!("Test task: {}", name)),
            ..Default::default()
        }
    }

    pub fn create_temp_project() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("env.cue"), "package test").unwrap();
        dir
    }
}
```
