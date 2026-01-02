//! Cubes sync provider.
//!
//! Syncs cube-generated files from CUE configuration.

use std::any::Any;
use std::path::Path;

use async_trait::async_trait;
use clap::{Arg, Command, arg};
use cuenv_core::Result;
use cuenv_core::manifest::{Base, Project};

use crate::commands::CommandExecutor;
use crate::commands::sync::functions;
use crate::commands::sync::provider::{SyncMode, SyncOptions, SyncResult};
use crate::provider::{Provider, SyncCapability};

/// Cubes sync provider.
///
/// Syncs cube-generated files from CUE configuration. Cubes are reusable
/// templates that generate project files.
///
/// # Example
///
/// ```ignore
/// use cuenv::Cuenv;
/// use cuenv::providers::CubesProvider;
///
/// Cuenv::builder()
///     .with_sync_provider(CubesProvider::new())
///     .build()
///     .run()
/// ```
pub struct CubesProvider;

impl CubesProvider {
    /// Create a new cubes provider.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for CubesProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl Provider for CubesProvider {
    fn name(&self) -> &'static str {
        "cubes"
    }

    fn description(&self) -> &'static str {
        "Sync files from CUE cube configurations"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

#[async_trait]
impl SyncCapability for CubesProvider {
    fn build_sync_command(&self) -> Command {
        Command::new(self.name())
            .about(self.description())
            .arg(arg!(-p --path <PATH> "Path to directory containing CUE files").default_value("."))
            .arg(
                Arg::new("package")
                    .long("package")
                    .help("Name of the CUE package to evaluate")
                    .default_value("cuenv"),
            )
            .arg(arg!(--"dry-run" "Show what would be generated without writing files"))
            .arg(arg!(--check "Check if files are in sync without making changes"))
            .arg(arg!(-A --all "Sync all projects in the workspace"))
            .arg(arg!(--diff "Show diff for files that would change"))
    }

    async fn sync_path(
        &self,
        path: &Path,
        package: &str,
        options: &SyncOptions,
        executor: &CommandExecutor,
    ) -> Result<SyncResult> {
        let dry_run = options.mode == SyncMode::DryRun;
        let check = options.mode == SyncMode::Check;

        let path_str = path.to_str().ok_or_else(|| {
            cuenv_core::Error::configuration(format!(
                "Path contains invalid UTF-8: {}",
                path.display()
            ))
        })?;

        let output = functions::execute_sync_cubes(
            path_str,
            package,
            dry_run,
            check,
            options.show_diff,
            Some(executor),
        )
        .await?;

        Ok(SyncResult::success(output))
    }

    async fn sync_workspace(
        &self,
        package: &str,
        options: &SyncOptions,
        executor: &CommandExecutor,
    ) -> Result<SyncResult> {
        let dry_run = options.mode == SyncMode::DryRun;
        let check = options.mode == SyncMode::Check;

        let cwd = std::env::current_dir().map_err(|e| {
            cuenv_core::Error::configuration(format!("Failed to get current directory: {e}"))
        })?;

        // Collect project info before any async operations
        let project_paths: Vec<(std::path::PathBuf, String)> = {
            let module = executor.get_module(&cwd)?;
            let mut paths = Vec::new();
            for instance in module.projects() {
                if let Ok(manifest) = instance.deserialize::<Project>()
                    && manifest.cube.is_some()
                {
                    paths.push((
                        module.root.join(&instance.path),
                        instance.path.display().to_string(),
                    ));
                }
            }
            paths
        };

        let mut outputs = Vec::new();
        let mut had_error = false;

        for (full_path, display_path) in project_paths {
            let Some(path_str) = full_path.to_str() else {
                outputs.push(format!(
                    "{}: Error: Path contains invalid UTF-8",
                    full_path.display()
                ));
                had_error = true;
                continue;
            };

            let result = functions::execute_sync_cubes(
                path_str,
                package,
                dry_run,
                check,
                options.show_diff,
                Some(executor),
            )
            .await;

            match result {
                Ok(output) if !output.is_empty() => {
                    let display = if display_path.is_empty() {
                        "[root]".to_string()
                    } else {
                        display_path
                    };
                    outputs.push(format!("{display}:\n{output}"));
                }
                Ok(_) => {}
                Err(e) => {
                    outputs.push(format!("{display_path}: Error: {e}"));
                    had_error = true;
                }
            }
        }

        if outputs.is_empty() {
            Ok(SyncResult::success("No cube configurations found."))
        } else {
            Ok(SyncResult {
                output: outputs.join("\n\n"),
                had_error,
            })
        }
    }

    fn has_config(&self, _manifest: &Base) -> bool {
        // Cubes are only in Projects, not Base
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cubes_provider_name() {
        let provider = CubesProvider::new();
        assert_eq!(provider.name(), "cubes");
    }

    #[test]
    fn test_cubes_provider_description() {
        let provider = CubesProvider::new();
        assert!(!provider.description().is_empty());
        assert!(provider.description().contains("cube"));
    }

    #[test]
    fn test_cubes_provider_as_any() {
        let provider = CubesProvider::new();
        let any = provider.as_any();
        assert!(any.is::<CubesProvider>());
    }

    #[test]
    fn test_cubes_provider_as_any_mut() {
        let mut provider = CubesProvider::new();
        let any = provider.as_any_mut();
        assert!(any.is::<CubesProvider>());
    }

    #[test]
    fn test_cubes_provider_command() {
        let provider = CubesProvider::new();
        let cmd = provider.build_sync_command();
        assert_eq!(cmd.get_name(), "cubes");
    }

    #[test]
    fn test_cubes_provider_command_has_args() {
        let provider = CubesProvider::new();
        let cmd = provider.build_sync_command();

        let args: Vec<_> = cmd.get_arguments().map(|a| a.get_id().as_str()).collect();
        assert!(args.contains(&"path"));
        assert!(args.contains(&"package"));
        assert!(args.contains(&"dry-run"));
        assert!(args.contains(&"check"));
        assert!(args.contains(&"all"));
        assert!(args.contains(&"diff"));
    }

    #[test]
    fn test_cubes_provider_default() {
        let provider = CubesProvider;
        assert_eq!(provider.name(), "cubes");
    }

    #[test]
    fn test_cubes_provider_has_config() {
        let provider = CubesProvider::new();
        let base = Base::default();
        // Cubes are only in Projects, not Base
        assert!(!provider.has_config(&base));
    }
}
