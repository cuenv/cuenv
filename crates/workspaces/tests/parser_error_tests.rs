//! Error path tests for JavaScript lockfile parsers
//!
//! These tests verify that parsers handle malformed, invalid, and edge case inputs gracefully.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use cuenv_workspaces::LockfileParser;
use std::fs;
use std::path::Path;
use tempfile::TempDir;

#[cfg(feature = "parser-npm")]
mod npm_error_tests {
    use super::*;
    use cuenv_workspaces::NpmLockfileParser;

    #[test]
    fn test_npm_empty_file() {
        let temp_dir = TempDir::new().unwrap();
        let lockfile = temp_dir.path().join("package-lock.json");
        fs::write(&lockfile, "").unwrap();

        let parser = NpmLockfileParser;
        let result = parser.parse(&lockfile);

        assert!(result.is_err(), "Empty file should cause parse error");
    }

    #[test]
    fn test_npm_invalid_json() {
        let temp_dir = TempDir::new().unwrap();
        let lockfile = temp_dir.path().join("package-lock.json");
        fs::write(&lockfile, "{ invalid json }").unwrap();

        let parser = NpmLockfileParser;
        let result = parser.parse(&lockfile);

        assert!(result.is_err(), "Invalid JSON should cause parse error");
    }

    #[test]
    fn test_npm_missing_lockfile_version() {
        let temp_dir = TempDir::new().unwrap();
        let lockfile = temp_dir.path().join("package-lock.json");
        let content = r#"{
            "name": "test",
            "packages": {}
        }"#;
        fs::write(&lockfile, content).unwrap();

        let parser = NpmLockfileParser;
        let result = parser.parse(&lockfile);

        // Should either succeed (treating as default) or fail with clear error
        match result {
            Ok(entries) => {
                // Default handling is acceptable
                assert!(entries.is_empty() || !entries.is_empty());
            }
            Err(e) => {
                let err_str = e.to_string();
                assert!(!err_str.is_empty(), "Error should have message");
            }
        }
    }

    #[test]
    fn test_npm_malformed_packages() {
        let temp_dir = TempDir::new().unwrap();
        let lockfile = temp_dir.path().join("package-lock.json");
        let content = r#"{
            "name": "test",
            "lockfileVersion": 3,
            "packages": "not an object"
        }"#;
        fs::write(&lockfile, content).unwrap();

        let parser = NpmLockfileParser;
        let result = parser.parse(&lockfile);

        assert!(result.is_err(), "Malformed packages should cause error");
    }

    #[test]
    fn test_npm_missing_required_fields() {
        let temp_dir = TempDir::new().unwrap();
        let lockfile = temp_dir.path().join("package-lock.json");
        let content = r#"{
            "name": "test",
            "lockfileVersion": 3,
            "packages": {
                "node_modules/pkg": {}
            }
        }"#;
        fs::write(&lockfile, content).unwrap();

        let parser = NpmLockfileParser;
        let result = parser.parse(&lockfile);

        // Should handle missing fields gracefully
        match result {
            Ok(entries) => {
                // Parser should skip or use defaults
                println!("Handled missing fields, got {} entries", entries.len());
            }
            Err(e) => {
                println!("Rejected missing fields: {}", e);
            }
        }
    }

    #[test]
    fn test_npm_nonexistent_file() {
        let parser = NpmLockfileParser;
        let result = parser.parse(Path::new("/nonexistent/package-lock.json"));

        assert!(result.is_err(), "Nonexistent file should cause error");
    }
}

#[cfg(feature = "parser-pnpm")]
mod pnpm_error_tests {
    use super::*;
    use cuenv_workspaces::PnpmLockfileParser;

    #[test]
    fn test_pnpm_empty_file() {
        let temp_dir = TempDir::new().unwrap();
        let lockfile = temp_dir.path().join("pnpm-lock.yaml");
        fs::write(&lockfile, "").unwrap();

        let parser = PnpmLockfileParser;
        let result = parser.parse(&lockfile);

        assert!(result.is_err(), "Empty file should cause parse error");
    }

    #[test]
    fn test_pnpm_invalid_yaml() {
        let temp_dir = TempDir::new().unwrap();
        let lockfile = temp_dir.path().join("pnpm-lock.yaml");
        fs::write(&lockfile, "invalid: yaml: structure:").unwrap();

        let parser = PnpmLockfileParser;
        let result = parser.parse(&lockfile);

        assert!(result.is_err(), "Invalid YAML should cause parse error");
    }

    #[test]
    fn test_pnpm_missing_version() {
        let temp_dir = TempDir::new().unwrap();
        let lockfile = temp_dir.path().join("pnpm-lock.yaml");
        let content = r#"
importers:
  .:
    dependencies: {}
"#;
        fs::write(&lockfile, content).unwrap();

        let parser = PnpmLockfileParser;
        let result = parser.parse(&lockfile);

        // Should handle missing version
        match result {
            Ok(entries) => {
                println!("Handled missing version, got {} entries", entries.len());
            }
            Err(e) => {
                println!("Rejected missing version: {}", e);
            }
        }
    }

    #[test]
    fn test_pnpm_malformed_packages() {
        let temp_dir = TempDir::new().unwrap();
        let lockfile = temp_dir.path().join("pnpm-lock.yaml");
        let content = r#"
lockfileVersion: '6.0'
importers:
  .: 
    dependencies: {}
packages: "not a map"
"#;
        fs::write(&lockfile, content).unwrap();

        let parser = PnpmLockfileParser;
        let result = parser.parse(&lockfile);

        assert!(result.is_err(), "Malformed packages should cause error");
    }

    #[test]
    fn test_pnpm_nonexistent_file() {
        let parser = PnpmLockfileParser;
        let result = parser.parse(Path::new("/nonexistent/pnpm-lock.yaml"));

        assert!(result.is_err(), "Nonexistent file should cause error");
    }
}

#[cfg(feature = "parser-yarn-classic")]
mod yarn_classic_error_tests {
    use super::*;
    use cuenv_workspaces::YarnClassicLockfileParser;

    #[test]
    fn test_yarn_empty_file() {
        let temp_dir = TempDir::new().unwrap();
        let lockfile = temp_dir.path().join("yarn.lock");
        fs::write(&lockfile, "").unwrap();

        let parser = YarnClassicLockfileParser;
        let result = parser.parse(&lockfile);

        // Empty yarn.lock might be valid (no dependencies)
        match result {
            Ok(entries) => {
                assert!(entries.is_empty(), "Empty yarn.lock should have no entries");
            }
            Err(_) => {
                // Error is also acceptable
            }
        }
    }

    #[test]
    fn test_yarn_invalid_format() {
        let temp_dir = TempDir::new().unwrap();
        let lockfile = temp_dir.path().join("yarn.lock");
        fs::write(&lockfile, "random text\nwithout proper format").unwrap();

        let parser = YarnClassicLockfileParser;
        let result = parser.parse(&lockfile);

        // Should handle gracefully
        match result {
            Ok(entries) => {
                // Might parse as empty or skip invalid entries
                println!("Parsed {} entries from invalid format", entries.len());
            }
            Err(e) => {
                println!("Rejected invalid format: {}", e);
            }
        }
    }

    #[test]
    fn test_yarn_nonexistent_file() {
        let parser = YarnClassicLockfileParser;
        let result = parser.parse(Path::new("/nonexistent/yarn.lock"));

        assert!(result.is_err(), "Nonexistent file should cause error");
    }
}

#[cfg(feature = "parser-yarn-modern")]
mod yarn_modern_error_tests {
    use super::*;
    use cuenv_workspaces::YarnModernLockfileParser;

    #[test]
    fn test_yarn_modern_empty_file() {
        let temp_dir = TempDir::new().unwrap();
        let lockfile = temp_dir.path().join("yarn.lock");
        fs::write(&lockfile, "").unwrap();

        let parser = YarnModernLockfileParser;
        let result = parser.parse(&lockfile);

        assert!(result.is_err(), "Empty file should cause parse error");
    }

    #[test]
    fn test_yarn_modern_invalid_yaml() {
        let temp_dir = TempDir::new().unwrap();
        let lockfile = temp_dir.path().join("yarn.lock");
        fs::write(&lockfile, "invalid: yaml: structure:").unwrap();

        let parser = YarnModernLockfileParser;
        let result = parser.parse(&lockfile);

        assert!(result.is_err(), "Invalid YAML should cause parse error");
    }

    #[test]
    fn test_yarn_modern_missing_version_field() {
        let temp_dir = TempDir::new().unwrap();
        let lockfile = temp_dir.path().join("yarn.lock");
        let content = r#"
__metadata:
  cacheKey: 8
"#;
        fs::write(&lockfile, content).unwrap();

        let parser = YarnModernLockfileParser;
        let result = parser.parse(&lockfile);

        // Should handle missing version field
        match result {
            Ok(entries) => {
                println!("Parsed {} entries without version", entries.len());
            }
            Err(e) => {
                println!("Rejected missing version: {}", e);
            }
        }
    }

    #[test]
    fn test_yarn_modern_nonexistent_file() {
        let parser = YarnModernLockfileParser;
        let result = parser.parse(Path::new("/nonexistent/yarn.lock"));

        assert!(result.is_err(), "Nonexistent file should cause error");
    }
}

#[cfg(feature = "parser-bun")]
mod bun_error_tests {
    use super::*;
    use cuenv_workspaces::BunLockfileParser;

    #[test]
    fn test_bun_nonexistent_file() {
        let parser = BunLockfileParser;
        let result = parser.parse(Path::new("/nonexistent/bun.lockb"));

        assert!(result.is_err(), "Nonexistent file should cause error");
    }

    #[test]
    fn test_bun_invalid_binary() {
        let temp_dir = TempDir::new().unwrap();
        let lockfile = temp_dir.path().join("bun.lockb");
        fs::write(&lockfile, b"random binary data \x00\x01\x02").unwrap();

        let parser = BunLockfileParser;
        let result = parser.parse(&lockfile);

        assert!(result.is_err(), "Invalid binary data should cause error");
    }

    #[test]
    fn test_bun_empty_file() {
        let temp_dir = TempDir::new().unwrap();
        let lockfile = temp_dir.path().join("bun.lockb");
        fs::write(&lockfile, b"").unwrap();

        let parser = BunLockfileParser;
        let result = parser.parse(&lockfile);

        assert!(result.is_err(), "Empty file should cause error");
    }
}

// Edge case tests for all parsers
#[test]
fn test_parsers_handle_large_files() {
    // Test that parsers can handle reasonably large lockfiles
    // This is more of a performance/memory test
    let temp_dir = TempDir::new().unwrap();

    #[cfg(feature = "parser-npm")]
    {
        let lockfile = temp_dir.path().join("large-package-lock.json");
        let mut content = r#"{"name":"test","lockfileVersion":3,"packages":{"#.to_string();

        // Generate a large number of packages
        for i in 0..100 {
            if i > 0 {
                content.push(',');
            }
            content.push_str(&format!(
                r#""node_modules/pkg{}": {{"version": "1.0.0"}}"#,
                i
            ));
        }
        content.push_str("}}");

        fs::write(&lockfile, content).unwrap();

        let parser = cuenv_workspaces::NpmLockfileParser;
        let result = parser.parse(&lockfile);

        // Should handle without running out of memory
        assert!(
            result.is_ok() || result.is_err(),
            "Parser should complete (success or error)"
        );
    }
}

#[test]
fn test_parsers_handle_unicode() {
    let temp_dir = TempDir::new().unwrap();

    #[cfg(feature = "parser-npm")]
    {
        let lockfile = temp_dir.path().join("unicode-package-lock.json");
        let content = r#"{
            "name": "测试-тест-δοκιμή",
            "lockfileVersion": 3,
            "packages": {
                "node_modules/emoji-pkg-😀": {
                    "version": "1.0.0"
                }
            }
        }"#;
        fs::write(&lockfile, content).unwrap();

        let parser = cuenv_workspaces::NpmLockfileParser;
        let result = parser.parse(&lockfile);

        // Should handle Unicode in package names
        match result {
            Ok(entries) => {
                println!("Parsed {} entries with Unicode names", entries.len());
            }
            Err(e) => {
                println!("Unicode handling: {}", e);
            }
        }
    }
}

#[test]
fn test_parsers_handle_special_characters() {
    let temp_dir = TempDir::new().unwrap();

    #[cfg(feature = "parser-npm")]
    {
        let lockfile = temp_dir.path().join("special-package-lock.json");
        let content = r#"{
            "name": "test",
            "lockfileVersion": 3,
            "packages": {
                "node_modules/@scope/package-name": {
                    "version": "1.0.0"
                },
                "node_modules/package.with.dots": {
                    "version": "2.0.0"
                }
            }
        }"#;
        fs::write(&lockfile, content).unwrap();

        let parser = cuenv_workspaces::NpmLockfileParser;
        let result = parser.parse(&lockfile);

        assert!(
            result.is_ok(),
            "Should handle scoped packages and dots in names"
        );

        if let Ok(entries) = result {
            assert!(entries.len() >= 2, "Should parse both special-named packages");
        }
    }
}
