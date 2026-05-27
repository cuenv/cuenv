//! Integration tests for JavaScript lockfile parsers.

use cuenv_workspaces::{DependencySource, LockfileEntry, LockfileParser};
use std::path::{Path, PathBuf};

const FIXTURES_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn fixture_path(name: &str) -> PathBuf {
    Path::new(FIXTURES_DIR).join(name)
}

fn parse_fixture<P: LockfileParser>(
    parser: &P,
    fixture_name: &str,
) -> TestResult<Vec<LockfileEntry>> {
    Ok(parser.parse(&fixture_path(fixture_name))?)
}

fn entry_named<'a>(entries: &'a [LockfileEntry], name: &str) -> TestResult<&'a LockfileEntry> {
    entries
        .iter()
        .find(|entry| entry.name == name)
        .ok_or_else(|| format!("dependency entry `{name}` not found").into())
}

fn workspace_entry_named<'a>(
    entries: &'a [LockfileEntry],
    name: &str,
) -> TestResult<&'a LockfileEntry> {
    entries
        .iter()
        .find(|entry| entry.name == name && entry.is_workspace_member)
        .ok_or_else(|| format!("workspace entry `{name}` not found").into())
}

fn first_dependency_name(entry: &LockfileEntry) -> TestResult<&str> {
    entry
        .dependencies
        .first()
        .map(|dependency| dependency.name.as_str())
        .ok_or_else(|| format!("entry `{}` has no dependencies", entry.name).into())
}

#[cfg(feature = "parser-npm")]
mod npm_tests {
    use super::*;
    use cuenv_workspaces::NpmLockfileParser;

    #[test]
    fn parses_npm_lockfile_fixture() -> TestResult {
        let parser = NpmLockfileParser;
        let entries = parse_fixture(&parser, "package-lock.json")?;

        // Should have workspace root + workspace member + external dependencies
        assert!(
            entries.len() >= 5,
            "Expected at least 5 entries, got {}",
            entries.len()
        );

        // Check workspace root
        let workspace_root = workspace_entry_named(&entries, "test-workspace")?;
        assert_eq!(workspace_root.version, "1.0.0");
        assert!(matches!(
            workspace_root.source,
            DependencySource::Workspace(_)
        ));
        assert!(!workspace_root.dependencies.is_empty());

        // Check workspace member
        let app = entry_named(&entries, "@test/app")?;
        assert!(app.is_workspace_member);
        assert_eq!(app.version, "0.1.0");
        assert!(matches!(app.source, DependencySource::Workspace(_)));

        // Check external dependency
        let lodash = entry_named(&entries, "lodash")?;
        assert!(!lodash.is_workspace_member);
        assert_eq!(lodash.version, "4.17.21");
        assert!(matches!(lodash.source, DependencySource::Registry(_)));
        assert!(lodash.checksum.is_some());

        // Check dependency with its own dependencies
        let react = entry_named(&entries, "react")?;
        assert!(!react.is_workspace_member);
        assert_eq!(react.version, "18.2.0");
        assert!(!react.dependencies.is_empty());
        assert_eq!(first_dependency_name(react)?, "loose-envify");
        Ok(())
    }

    #[test]
    fn npm_parser_supports_correct_filename() {
        let parser = NpmLockfileParser;
        assert!(parser.supports_lockfile(Path::new("package-lock.json")));
        assert!(!parser.supports_lockfile(Path::new("bun.lock")));
        assert_eq!(parser.lockfile_name(), "package-lock.json");
    }
}

#[cfg(feature = "parser-bun")]
mod bun_tests {
    use super::*;
    use cuenv_workspaces::BunLockfileParser;

    #[test]
    fn parses_bun_lockfile_fixture() -> TestResult {
        let parser = BunLockfileParser;
        let entries = parse_fixture(&parser, "bun.lock")?;

        // Should have 2 workspace members + 3 packages
        assert!(
            entries.len() >= 5,
            "Expected at least 5 entries, got {}",
            entries.len()
        );

        // Check workspace root
        let workspace_root = workspace_entry_named(&entries, "test-workspace")?;
        assert_eq!(workspace_root.version, "1.0.0");
        assert!(matches!(
            workspace_root.source,
            DependencySource::Workspace(_)
        ));

        // Check workspace member
        let app = entry_named(&entries, "@test/app")?;
        assert!(app.is_workspace_member);
        assert_eq!(app.version, "0.1.0");

        // Check external dependency
        let lodash = entry_named(&entries, "lodash")?;
        assert!(!lodash.is_workspace_member);
        assert_eq!(lodash.version, "4.17.21");
        assert!(matches!(lodash.source, DependencySource::Registry(_)));
        assert!(lodash.checksum.is_some());

        // Check dependency with dependencies
        let react = entry_named(&entries, "react")?;
        assert!(!react.is_workspace_member);
        assert_eq!(react.version, "18.2.0");
        assert!(!react.dependencies.is_empty());
        Ok(())
    }

    #[test]
    fn bun_parser_supports_correct_filename() {
        let parser = BunLockfileParser;
        assert!(parser.supports_lockfile(Path::new("bun.lock")));
        assert!(!parser.supports_lockfile(Path::new("bun.lockb")));
        assert!(!parser.supports_lockfile(Path::new("package-lock.json")));
        assert_eq!(parser.lockfile_name(), "bun.lock");
    }
}

#[cfg(feature = "parser-pnpm")]
mod pnpm_tests {
    use super::*;
    use cuenv_workspaces::PnpmLockfileParser;

    #[test]
    fn parses_pnpm_lockfile_fixture() -> TestResult {
        let parser = PnpmLockfileParser;
        let entries = parse_fixture(&parser, "pnpm-lock.yaml")?;

        // Should have 2 workspace importers + 4 packages
        assert!(
            entries.len() >= 6,
            "Expected at least 6 entries, got {}",
            entries.len()
        );

        // Check workspace root
        let workspace_count = entries.iter().filter(|e| e.is_workspace_member).count();
        assert!(
            workspace_count >= 2,
            "Expected at least 2 workspace members"
        );

        // Check external dependency
        let lodash = entry_named(&entries, "lodash")?;
        assert!(!lodash.is_workspace_member);
        assert_eq!(lodash.version, "4.17.21");
        assert!(matches!(lodash.source, DependencySource::Registry(_)));
        assert!(lodash.checksum.is_some());

        // Check dependency with dependencies
        let react = entry_named(&entries, "react")?;
        assert!(!react.is_workspace_member);
        assert_eq!(react.version, "18.2.0");
        assert!(!react.dependencies.is_empty());
        assert_eq!(first_dependency_name(react)?, "loose-envify");
        Ok(())
    }

    #[test]
    fn pnpm_parser_supports_correct_filename() {
        let parser = PnpmLockfileParser;
        assert!(parser.supports_lockfile(Path::new("pnpm-lock.yaml")));
        assert!(!parser.supports_lockfile(Path::new("package-lock.json")));
        assert_eq!(parser.lockfile_name(), "pnpm-lock.yaml");
    }
}

#[cfg(feature = "parser-yarn-classic")]
mod yarn_classic_tests {
    use super::*;
    use cuenv_workspaces::YarnClassicLockfileParser;

    #[test]
    fn parses_yarn_classic_lockfile_fixture() -> TestResult {
        let parser = YarnClassicLockfileParser;
        let entries = parse_fixture(&parser, "yarn-classic.lock")?;

        // Should have 4 packages
        assert!(
            entries.len() >= 4,
            "Expected at least 4 entries, got {}",
            entries.len()
        );

        // Check external dependency
        let lodash = entry_named(&entries, "lodash")?;
        assert!(!lodash.is_workspace_member);
        assert_eq!(lodash.version, "4.17.21");
        assert!(matches!(lodash.source, DependencySource::Registry(_)));
        assert!(lodash.checksum.is_some());

        // Check dependency with dependencies
        let react = entry_named(&entries, "react")?;
        assert!(!react.is_workspace_member);
        assert_eq!(react.version, "18.2.0");
        assert!(!react.dependencies.is_empty());
        assert_eq!(first_dependency_name(react)?, "loose-envify");
        Ok(())
    }

    #[test]
    fn yarn_classic_parser_supports_correct_filename() {
        let parser = YarnClassicLockfileParser;
        assert!(parser.supports_lockfile(Path::new("yarn.lock")));
        assert!(!parser.supports_lockfile(Path::new("package-lock.json")));
        assert_eq!(parser.lockfile_name(), "yarn.lock");
    }

    #[test]
    fn yarn_classic_parser_detects_v1_lockfile_via_content_sniffing() -> TestResult {
        use std::fs;
        use tempfile::TempDir;

        // Create a unique temp directory with a yarn.lock file containing v1 content
        let temp_dir = TempDir::new()?;
        let yarn_lock_path = temp_dir.path().join("yarn.lock");

        let classic_content = fs::read_to_string(fixture_path("yarn-classic.lock"))?;
        fs::write(&yarn_lock_path, classic_content)?;

        let parser = YarnClassicLockfileParser;

        // Yarn Classic parser should accept v1 lockfile based on content
        assert!(
            parser.supports_lockfile(&yarn_lock_path),
            "Yarn Classic parser should accept v1 lockfile"
        );
        Ok(())
    }

    #[test]
    fn yarn_classic_parser_rejects_modern_lockfile_via_content_sniffing() -> TestResult {
        use std::fs;
        use tempfile::TempDir;

        // Create a unique temp directory with a yarn.lock file containing v2 content
        let temp_dir = TempDir::new()?;
        let yarn_lock_path = temp_dir.path().join("yarn.lock");

        let modern_content = fs::read_to_string(fixture_path("yarn-modern.lock"))?;
        fs::write(&yarn_lock_path, modern_content)?;

        let parser = YarnClassicLockfileParser;

        // Yarn Classic parser should reject v2+ lockfile based on content
        assert!(
            !parser.supports_lockfile(&yarn_lock_path),
            "Yarn Classic parser should reject v2+ lockfile with __metadata"
        );
        Ok(())
    }
}

#[cfg(feature = "parser-yarn-modern")]
mod yarn_modern_tests {
    use super::*;
    use cuenv_workspaces::YarnModernLockfileParser;

    #[test]
    fn parses_yarn_modern_lockfile_fixture() -> TestResult {
        let parser = YarnModernLockfileParser;
        let entries = parse_fixture(&parser, "yarn-modern.lock")?;

        // Should have entries for packages and workspace members
        assert!(
            entries.len() >= 5,
            "Expected at least 5 entries, got {}",
            entries.len()
        );

        // Check workspace root
        let workspace_root = workspace_entry_named(&entries, "test-workspace")?;
        assert!(matches!(
            workspace_root.source,
            DependencySource::Workspace(_)
        ));

        // Check workspace member
        let app = entry_named(&entries, "@test/app")?;
        assert!(app.is_workspace_member);
        assert!(matches!(app.source, DependencySource::Workspace(_)));

        // Check external dependency
        let lodash = entry_named(&entries, "lodash")?;
        assert!(!lodash.is_workspace_member);
        assert_eq!(lodash.version, "4.17.21");
        assert!(matches!(lodash.source, DependencySource::Registry(_)));
        assert!(lodash.checksum.is_some());

        // Check dependency with dependencies
        let react = entry_named(&entries, "react")?;
        assert!(!react.is_workspace_member);
        assert_eq!(react.version, "18.2.0");
        assert!(!react.dependencies.is_empty());
        Ok(())
    }

    #[test]
    fn yarn_modern_parser_supports_correct_filename() {
        let parser = YarnModernLockfileParser;
        assert!(parser.supports_lockfile(Path::new("yarn.lock")));
        assert!(!parser.supports_lockfile(Path::new("package-lock.json")));
        assert_eq!(parser.lockfile_name(), "yarn.lock");
    }

    #[test]
    fn yarn_modern_parser_detects_v2_lockfile_via_content_sniffing() -> TestResult {
        use std::fs;
        use tempfile::TempDir;

        // Create a unique temp directory with a yarn.lock file containing v2 content
        let temp_dir = TempDir::new()?;
        let yarn_lock_path = temp_dir.path().join("yarn.lock");

        let modern_content = fs::read_to_string(fixture_path("yarn-modern.lock"))?;
        fs::write(&yarn_lock_path, modern_content)?;

        let parser = YarnModernLockfileParser;

        // Yarn Modern parser should accept v2+ lockfile based on content
        assert!(
            parser.supports_lockfile(&yarn_lock_path),
            "Yarn Modern parser should accept v2+ lockfile with __metadata"
        );
        Ok(())
    }

    #[test]
    fn yarn_modern_parser_rejects_classic_lockfile_via_content_sniffing() -> TestResult {
        use std::fs;
        use tempfile::TempDir;

        // Create a unique temp directory with a yarn.lock file containing v1 content
        let temp_dir = TempDir::new()?;
        let yarn_lock_path = temp_dir.path().join("yarn.lock");

        let classic_content = fs::read_to_string(fixture_path("yarn-classic.lock"))?;
        fs::write(&yarn_lock_path, classic_content)?;

        let parser = YarnModernLockfileParser;

        // Yarn Modern parser should reject v1 lockfile based on content
        assert!(
            !parser.supports_lockfile(&yarn_lock_path),
            "Yarn Modern parser should reject v1 lockfile"
        );
        Ok(())
    }
}
