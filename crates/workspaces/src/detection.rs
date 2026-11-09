//! Package manager detection module.
//!
//! This module provides functionality to detect which package managers are in use
//! within a workspace by scanning for lockfiles, workspace configurations, and
//! analyzing command strings.
//!
//! # Examples
//!
//! Detect package managers in a directory:
//!
//! ```rust
//! use cuenv_workspaces::{PackageManager, detection};
//! let root = tempfile::tempdir()?;
//! std::fs::write(root.path().join("package-lock.json"), "{}")?;
//! let managers = detection::detect_package_managers(root.path())?;
//! assert!(managers.contains(&PackageManager::Npm));
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! Detect from a command string:
//!
//! ```
//! use cuenv_workspaces::detection;
//! use cuenv_workspaces::PackageManager;
//!
//! assert_eq!(detection::detect_from_command("cargo build"), Some(PackageManager::Cargo));
//! assert_eq!(detection::detect_from_command("npm install"), Some(PackageManager::Npm));
//! assert_eq!(detection::detect_from_command("bun run test"), Some(PackageManager::Bun));
//! ```

mod command;
mod detector;
mod filesystem;
mod package_json;
mod scoring;

pub use command::{command_name, detect_from_command};
pub use detector::{detect_package_managers, detect_with_command_hint};

#[cfg(test)]
mod tests {
    use super::scoring::{Detection, DetectionSignals};
    use super::*;
    use crate::core::types::PackageManager;
    use crate::error::Error;
    use std::fs;
    use std::io::BufReader;
    use std::path::{Path, PathBuf};
    use tempfile::TempDir;

    // Helper function to create a test workspace directory
    fn create_test_workspace() -> TempDir {
        TempDir::new().expect("Failed to create temp dir")
    }

    // Helper function to create a lockfile
    fn create_lockfile(dir: &Path, manager: PackageManager) -> PathBuf {
        let lockfile_path = dir.join(manager.lockfile_name());
        let content = match manager {
            PackageManager::Npm => r#"{"lockfileVersion": 2}"#,
            PackageManager::Bun => "binary content",
            PackageManager::Pnpm => "lockfileVersion: '6.0'",
            PackageManager::YarnClassic => "# yarn lockfile v1\n",
            PackageManager::YarnModern => "__metadata:\n  version: 6\n",
            PackageManager::Cargo => "[root]\n",
            PackageManager::Deno => r#"{"version": "3"}"#,
        };
        fs::write(&lockfile_path, content).expect("Failed to write lockfile");
        lockfile_path
    }

    // Helper function to create a workspace config
    fn create_workspace_config(dir: &Path, manager: PackageManager) -> PathBuf {
        let config_path = dir.join(manager.workspace_config_name());
        let content = match manager {
            PackageManager::Npm
            | PackageManager::Bun
            | PackageManager::YarnClassic
            | PackageManager::YarnModern => r#"{"name": "test", "workspaces": ["packages/*"]}"#,
            PackageManager::Pnpm => "packages:\n  - 'packages/*'\n",
            PackageManager::Cargo => "[workspace]\nmembers = [\"crates/*\"]\n",
            PackageManager::Deno => r#"{"name": "test", "workspace": ["packages/*"]}"#,
        };
        fs::write(&config_path, content).expect("Failed to write config");
        config_path
    }

    #[test]
    fn test_calculate_confidence() {
        let cases = [
            (true, true, 100),
            (true, false, 75),
            (false, true, 50),
            (false, false, 0),
        ];
        for (has_lockfile, has_valid_config, expected) in cases {
            assert_eq!(
                super::scoring::calculate_confidence(DetectionSignals {
                    has_lockfile,
                    has_valid_config,
                }),
                expected
            );
        }
    }

    #[test]
    fn test_prioritize_managers() {
        let detections = vec![
            Detection::with_confidence(PackageManager::Npm, 75),
            Detection::with_confidence(PackageManager::Cargo, 100),
            Detection::with_confidence(PackageManager::Bun, 75),
        ];

        let result = super::scoring::prioritize_managers(detections);

        assert_eq!(result[0], PackageManager::Cargo); // Highest confidence
        assert_eq!(result[1], PackageManager::Bun); // Same confidence as npm, but higher priority
        assert_eq!(result[2], PackageManager::Npm);
    }

    #[test]
    fn test_prioritize_managers_equal_confidence() {
        let detections = vec![
            Detection::with_confidence(PackageManager::Npm, 75),
            Detection::with_confidence(PackageManager::Bun, 75),
            Detection::with_confidence(PackageManager::Cargo, 75),
        ];

        let result = super::scoring::prioritize_managers(detections);

        // With equal confidence, should be sorted by priority: Cargo > Bun > npm
        assert_eq!(result[0], PackageManager::Cargo);
        assert_eq!(result[1], PackageManager::Bun);
        assert_eq!(result[2], PackageManager::Npm);
    }

    #[test]
    fn test_detect_from_command() {
        assert_eq!(
            super::command::detect_from_command("cargo"),
            Some(PackageManager::Cargo)
        );
        assert_eq!(
            super::command::detect_from_command("npm"),
            Some(PackageManager::Npm)
        );
        assert_eq!(
            super::command::detect_from_command("npx"),
            Some(PackageManager::Npm)
        );
        assert_eq!(
            super::command::detect_from_command("bun"),
            Some(PackageManager::Bun)
        );
        assert_eq!(
            super::command::detect_from_command("bunx"),
            Some(PackageManager::Bun)
        );
        assert_eq!(
            super::command::detect_from_command("pnpm"),
            Some(PackageManager::Pnpm)
        );
        assert_eq!(
            super::command::detect_from_command("deno"),
            Some(PackageManager::Deno)
        );
        assert_eq!(
            super::command::detect_from_command("yarn"),
            Some(PackageManager::YarnClassic)
        );
        assert_eq!(
            super::command::detect_from_command("node"),
            Some(PackageManager::Npm)
        );
    }

    #[test]
    fn test_command_name_skips_env_prefixes() {
        assert_eq!(
            command_name("FOO=bar cargo test"),
            Some("cargo".to_string())
        );
        assert_eq!(
            command_name("env FOO=bar bun run build"),
            Some("bun".to_string())
        );
        assert_eq!(
            command_name("env -i CARGO_TERM_COLOR=always cargo test"),
            Some("cargo".to_string())
        );
        assert_eq!(
            command_name("env -- BUN_ENV=1 bun run lint"),
            Some("bun".to_string())
        );
        assert_eq!(command_name(""), None);
        assert_eq!(command_name("FOO=bar"), None);
    }

    #[test]
    fn test_command_name_respects_env_option_terminator() {
        assert_eq!(
            command_name("env -- -custom-command --flag"),
            Some("-custom-command".to_string())
        );
    }

    #[test]
    fn test_command_name_respects_shell_quoting() {
        assert_eq!(
            command_name(r#"env "BUN_ENV=space value" bun run test"#),
            Some("bun".to_string())
        );
        assert_eq!(command_name(r#""cargo" build"#), Some("cargo".to_string()));
        assert_eq!(command_name(r#""unterminated"#), None);
    }

    #[test]
    fn test_detect_from_command_with_args() {
        assert_eq!(
            super::command::detect_from_command("cargo build"),
            Some(PackageManager::Cargo)
        );
        assert_eq!(
            super::command::detect_from_command("npm install"),
            Some(PackageManager::Npm)
        );
        assert_eq!(
            super::command::detect_from_command("bun run test"),
            Some(PackageManager::Bun)
        );
        assert_eq!(
            super::command::detect_from_command("bunx eslint"),
            Some(PackageManager::Bun)
        );
        assert_eq!(
            super::command::detect_from_command("npx prisma generate"),
            Some(PackageManager::Npm)
        );
        assert_eq!(
            super::command::detect_from_command("FOO=bar cargo build"),
            Some(PackageManager::Cargo)
        );
        assert_eq!(
            super::command::detect_from_command("env -i BUN_ENV=1 bun run test"),
            Some(PackageManager::Bun)
        );
    }

    #[test]
    fn test_detect_from_command_unknown() {
        assert_eq!(super::command::detect_from_command("unknown"), None);
        assert_eq!(super::command::detect_from_command("make"), None);
        assert_eq!(super::command::detect_from_command("python"), None);
    }

    #[test]
    fn test_detect_yarn_classic() {
        let temp_dir = create_test_workspace();
        let lockfile_path = temp_dir.path().join("yarn.lock");

        let classic_content = r#"# yarn lockfile v1
# THIS IS AN AUTOGENERATED FILE. DO NOT EDIT THIS FILE DIRECTLY.

package@^1.0.0:
  version "1.0.0"
"#;
        fs::write(&lockfile_path, classic_content).unwrap();

        let result = super::filesystem::detect_yarn_version(&lockfile_path).unwrap();
        assert_eq!(result, PackageManager::YarnClassic);
    }

    #[test]
    fn test_detect_yarn_modern() {
        let temp_dir = create_test_workspace();
        let lockfile_path = temp_dir.path().join("yarn.lock");

        let modern_content = r#"__metadata:
  version: 6
  cacheKey: 8

"package@npm:^1.0.0":
  version: 1.0.0
"#;
        fs::write(&lockfile_path, modern_content).unwrap();

        let result = super::filesystem::detect_yarn_version(&lockfile_path).unwrap();
        assert_eq!(result, PackageManager::YarnModern);
    }

    #[test]
    fn test_detect_yarn_modern_metadata_within_first_five_lines() {
        let temp_dir = create_test_workspace();
        let lockfile_path = temp_dir.path().join("yarn.lock");

        let modern_content = r"# generated by yarn
# do not edit

__metadata:
  version: 8
";
        fs::write(&lockfile_path, modern_content).unwrap();

        let result = super::filesystem::detect_yarn_version(&lockfile_path).unwrap();
        assert_eq!(result, PackageManager::YarnModern);
    }

    #[test]
    fn test_read_first_lines_respects_limit() {
        let reader = BufReader::new(&b"one\ntwo\nthree\nfour\n"[..]);

        let lines = super::filesystem::read_first_lines(reader, 2, Path::new("yarn.lock")).unwrap();

        assert_eq!(lines, vec!["one".to_string(), "two".to_string()]);
    }

    #[test]
    fn test_find_lockfiles_cargo() {
        let temp_dir = create_test_workspace();
        create_lockfile(temp_dir.path(), PackageManager::Cargo);

        let result = super::filesystem::find_lockfiles(temp_dir.path());

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, PackageManager::Cargo);
    }

    #[test]
    fn test_find_lockfiles_npm() {
        let temp_dir = create_test_workspace();
        create_lockfile(temp_dir.path(), PackageManager::Npm);

        let result = super::filesystem::find_lockfiles(temp_dir.path());

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, PackageManager::Npm);
    }

    #[test]
    fn test_find_lockfiles_multiple() {
        let temp_dir = create_test_workspace();
        create_lockfile(temp_dir.path(), PackageManager::Cargo);
        create_lockfile(temp_dir.path(), PackageManager::Npm);

        let result = super::filesystem::find_lockfiles(temp_dir.path());

        assert_eq!(result.len(), 2);
        let managers: Vec<_> = result.iter().map(|(m, _)| *m).collect();
        assert!(managers.contains(&PackageManager::Cargo));
        assert!(managers.contains(&PackageManager::Npm));
    }

    #[test]
    fn test_find_lockfiles_none() {
        let temp_dir = create_test_workspace();

        let result = super::filesystem::find_lockfiles(temp_dir.path());

        assert_eq!(result.len(), 0);
    }

    #[test]
    fn test_find_lockfiles_bun_text() {
        let temp_dir = create_test_workspace();

        fs::write(temp_dir.path().join("bun.lock"), "text").unwrap();

        let result = super::filesystem::find_lockfiles(temp_dir.path());

        // Should detect the text bun.lock
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, PackageManager::Bun);
        assert_eq!(result[0].1.file_name().unwrap(), "bun.lock");
    }

    #[test]
    fn test_validate_workspace_config_cargo() {
        let temp_dir = create_test_workspace();
        create_workspace_config(temp_dir.path(), PackageManager::Cargo);

        let result =
            super::filesystem::validate_workspace_config(temp_dir.path(), PackageManager::Cargo)
                .unwrap();
        assert!(result);
    }

    #[test]
    fn test_validate_workspace_config_package_json() {
        let temp_dir = create_test_workspace();
        create_workspace_config(temp_dir.path(), PackageManager::Npm);

        let result =
            super::filesystem::validate_workspace_config(temp_dir.path(), PackageManager::Npm)
                .unwrap();
        assert!(result);
    }

    #[test]
    fn test_validate_workspace_config_pnpm() {
        let temp_dir = create_test_workspace();
        create_workspace_config(temp_dir.path(), PackageManager::Pnpm);

        let result =
            super::filesystem::validate_workspace_config(temp_dir.path(), PackageManager::Pnpm)
                .unwrap();
        assert!(result);
    }

    #[test]
    fn test_validate_workspace_config_invalid_json() {
        let temp_dir = create_test_workspace();
        let config_path = temp_dir.path().join("package.json");
        fs::write(&config_path, "{ invalid json }").unwrap();

        let result =
            super::filesystem::validate_workspace_config(temp_dir.path(), PackageManager::Npm);
        assert!(result.is_err());
        assert!(matches!(result, Err(Error::InvalidWorkspaceConfig { .. })));
    }

    #[test]
    fn test_validate_workspace_config_invalid_toml() {
        let temp_dir = create_test_workspace();
        let config_path = temp_dir.path().join("Cargo.toml");
        fs::write(&config_path, "[invalid toml").unwrap();

        let result =
            super::filesystem::validate_workspace_config(temp_dir.path(), PackageManager::Cargo);
        assert!(result.is_err());
        assert!(matches!(result, Err(Error::InvalidWorkspaceConfig { .. })));
    }

    #[test]
    fn test_validate_workspace_config_invalid_yaml() {
        let temp_dir = create_test_workspace();
        let config_path = temp_dir.path().join("pnpm-workspace.yaml");
        fs::write(&config_path, "invalid: yaml: content:").unwrap();

        let result =
            super::filesystem::validate_workspace_config(temp_dir.path(), PackageManager::Pnpm);
        assert!(result.is_err());
        assert!(matches!(result, Err(Error::InvalidWorkspaceConfig { .. })));
    }

    #[test]
    fn test_validate_workspace_config_missing() {
        let temp_dir = create_test_workspace();

        let result =
            super::filesystem::validate_workspace_config(temp_dir.path(), PackageManager::Npm)
                .unwrap();
        assert!(!result); // Should return false, not error
    }

    #[test]
    fn test_detect_package_managers_cargo() {
        let temp_dir = create_test_workspace();
        create_lockfile(temp_dir.path(), PackageManager::Cargo);
        create_workspace_config(temp_dir.path(), PackageManager::Cargo);

        let result = super::detector::detect_package_managers(temp_dir.path()).unwrap();

        assert_eq!(result.len(), 1);
        assert_eq!(result[0], PackageManager::Cargo);
    }

    #[test]
    fn test_detect_package_managers_npm() {
        let temp_dir = create_test_workspace();
        create_lockfile(temp_dir.path(), PackageManager::Npm);
        create_workspace_config(temp_dir.path(), PackageManager::Npm);

        let result = super::detector::detect_package_managers(temp_dir.path()).unwrap();

        assert_eq!(result, vec![PackageManager::Npm]);
    }

    #[test]
    fn test_detect_package_managers_multi() {
        let temp_dir = create_test_workspace();

        // Create both Cargo and Bun files
        create_lockfile(temp_dir.path(), PackageManager::Cargo);
        create_workspace_config(temp_dir.path(), PackageManager::Cargo);

        // For Bun, we need to create package.json (same as npm config)
        fs::write(
            temp_dir.path().join("package.json"),
            r#"{"name": "test", "workspaces": ["packages/*"]}"#,
        )
        .unwrap();
        create_lockfile(temp_dir.path(), PackageManager::Bun);

        let result = super::detector::detect_package_managers(temp_dir.path()).unwrap();

        // Cargo (lockfile + config = 100) and Bun (lockfile + config = 100) are detected
        // We should have at least 2 with Cargo and Bun first (sorted by priority)
        assert!(result.len() >= 2);
        // Cargo and Bun both have confidence 100, sorted by priority: Cargo > Bun
        assert_eq!(result[0], PackageManager::Cargo);
        assert_eq!(result[1], PackageManager::Bun);
    }

    #[test]
    fn test_detect_package_managers_lockfile_only() {
        let temp_dir = create_test_workspace();
        create_lockfile(temp_dir.path(), PackageManager::Cargo);
        // No workspace config

        let result = super::detector::detect_package_managers(temp_dir.path()).unwrap();

        assert_eq!(result.len(), 1);
        assert_eq!(result[0], PackageManager::Cargo);
    }

    #[test]
    fn test_detect_package_managers_config_only() {
        let temp_dir = create_test_workspace();
        create_workspace_config(temp_dir.path(), PackageManager::Cargo);
        // No lockfile

        let result = super::detector::detect_package_managers(temp_dir.path()).unwrap();

        assert_eq!(result.len(), 1);
        assert_eq!(result[0], PackageManager::Cargo);
    }

    #[test]
    fn test_detect_package_managers_empty_dir() {
        let temp_dir = create_test_workspace();

        let result = super::detector::detect_package_managers(temp_dir.path()).unwrap();

        assert_eq!(result.len(), 0);
    }

    #[test]
    fn test_detect_with_command_hint() {
        let temp_dir = create_test_workspace();

        // Create both Cargo and Bun files
        create_lockfile(temp_dir.path(), PackageManager::Cargo);
        create_workspace_config(temp_dir.path(), PackageManager::Cargo);

        fs::write(
            temp_dir.path().join("package.json"),
            r#"{"name": "test", "workspaces": ["packages/*"]}"#,
        )
        .unwrap();
        create_lockfile(temp_dir.path(), PackageManager::Bun);

        // Without hint, Cargo comes first (higher priority)
        let result = super::detector::detect_package_managers(temp_dir.path()).unwrap();
        assert_eq!(result[0], PackageManager::Cargo);

        // With Bun hint, Bun should come first
        let result =
            super::detector::detect_with_command_hint(temp_dir.path(), Some("bun run test"))
                .unwrap();
        assert_eq!(result[0], PackageManager::Bun);
        assert_eq!(result[1], PackageManager::Cargo);
    }

    #[test]
    fn test_detect_with_command_hint_not_detected() {
        let temp_dir = create_test_workspace();
        create_lockfile(temp_dir.path(), PackageManager::Cargo);
        create_workspace_config(temp_dir.path(), PackageManager::Cargo);

        // Hint for a manager that's not detected - should not affect results
        let result =
            super::detector::detect_with_command_hint(temp_dir.path(), Some("npm install"))
                .unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], PackageManager::Cargo);
    }

    #[test]
    fn test_detect_with_no_command_hint() {
        let temp_dir = create_test_workspace();
        create_lockfile(temp_dir.path(), PackageManager::Cargo);

        let result = super::detector::detect_with_command_hint(temp_dir.path(), None).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], PackageManager::Cargo);
    }

    #[test]
    fn test_yarn_version_detection_in_detect_package_managers() {
        let temp_dir = create_test_workspace();

        // Create a Yarn Modern lockfile
        let lockfile_path = temp_dir.path().join("yarn.lock");
        fs::write(&lockfile_path, "__metadata:\n  version: 6\n").unwrap();
        create_workspace_config(temp_dir.path(), PackageManager::YarnModern);

        let result = super::detector::detect_package_managers(temp_dir.path()).unwrap();

        assert_eq!(result, vec![PackageManager::YarnModern]);
    }

    #[test]
    fn test_package_json_config_only_defaults_to_npm() {
        let temp_dir = create_test_workspace();

        fs::write(
            temp_dir.path().join("package.json"),
            r#"{"name":"example"}"#,
        )
        .unwrap();

        let result = super::detector::detect_package_managers(temp_dir.path()).unwrap();

        assert_eq!(result, vec![PackageManager::Npm]);
    }

    #[test]
    fn test_package_json_package_manager_hint_yarn_classic() {
        let temp_dir = create_test_workspace();

        fs::write(
            temp_dir.path().join("package.json"),
            r#"{"name":"example","packageManager":"yarn@1.22.0"}"#,
        )
        .unwrap();

        let result = super::detector::detect_package_managers(temp_dir.path()).unwrap();

        assert_eq!(result, vec![PackageManager::YarnClassic]);
    }

    #[test]
    fn test_package_json_package_manager_hint_yarn_modern() {
        let temp_dir = create_test_workspace();

        fs::write(
            temp_dir.path().join("package.json"),
            r#"{"name":"example","packageManager":"yarn@3.5.1"}"#,
        )
        .unwrap();

        let result = super::detector::detect_package_managers(temp_dir.path()).unwrap();

        assert_eq!(result, vec![PackageManager::YarnModern]);
    }

    #[test]
    fn test_package_json_package_manager_hint_bun() {
        let temp_dir = create_test_workspace();

        fs::write(
            temp_dir.path().join("package.json"),
            r#"{"name":"example","packageManager":"bun@1.0.0"}"#,
        )
        .unwrap();

        let result = super::detector::detect_package_managers(temp_dir.path()).unwrap();

        assert_eq!(result, vec![PackageManager::Bun]);
    }

    #[test]
    fn test_yarn_modern_lockfile_does_not_duplicate_classic() {
        let temp_dir = create_test_workspace();
        create_lockfile(temp_dir.path(), PackageManager::YarnModern);
        fs::write(
            temp_dir.path().join("package.json"),
            r#"{"name":"example"}"#,
        )
        .unwrap();

        let result = super::detector::detect_package_managers(temp_dir.path()).unwrap();

        assert_eq!(result, vec![PackageManager::YarnModern]);
    }

    #[test]
    fn test_manager_priority() {
        assert!(
            super::scoring::manager_priority(PackageManager::Cargo)
                < super::scoring::manager_priority(PackageManager::Deno)
        );
        assert!(
            super::scoring::manager_priority(PackageManager::Deno)
                < super::scoring::manager_priority(PackageManager::Bun)
        );
        assert!(
            super::scoring::manager_priority(PackageManager::Bun)
                < super::scoring::manager_priority(PackageManager::Pnpm)
        );
        assert!(
            super::scoring::manager_priority(PackageManager::Pnpm)
                < super::scoring::manager_priority(PackageManager::YarnModern)
        );
        assert!(
            super::scoring::manager_priority(PackageManager::YarnModern)
                < super::scoring::manager_priority(PackageManager::YarnClassic)
        );
        assert!(
            super::scoring::manager_priority(PackageManager::YarnClassic)
                < super::scoring::manager_priority(PackageManager::Npm)
        );
    }
}
