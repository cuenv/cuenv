//! Hook execution engine with background processing and state management.

mod hook_executor;
mod output;
mod source_environment;

pub use hook_executor::{
    HookExecutor, HookRunOptions, HookRunState, HookSessionContext, execute_hooks,
    execute_hooks_with_options,
};
pub use output::StderrLines;
pub use source_environment::{
    SourceEnvironmentOptions, capture_source_environment, capture_source_environment_with_options,
};

#[cfg(test)]
use hook_executor::duration_millis;
#[cfg(test)]
#[path = "executor_tests.rs"]
mod tests;
