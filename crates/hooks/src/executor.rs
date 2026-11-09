//! Hook execution engine with background processing and state management.

mod hook_executor;
mod output;
mod source_environment;

pub use hook_executor::{HookExecutor, HookRunState, HookSessionContext, execute_hooks};
pub use source_environment::capture_source_environment;

#[cfg(test)]
use hook_executor::duration_millis;
#[cfg(test)]
#[path = "executor_tests.rs"]
mod tests;
