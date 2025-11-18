//! Integration tests for JavaScript lockfile parsers.

use cuenv_workspaces::{DependencySource, LockfileParser};
use std::path::Path;

const FIXTURES_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

#[cfg(feature = "parser-npm")]
mod npm_tests {
    use super::*;
    use cuenv_workspaces::NpmLockfileParser;

    #[test]
    fn parses_npm_lockfile_fixture() {
        let fixture_path = Path::new(FIXTURES_DIR).join("package-lock.json");
        let parser = NpmLockfileParser;

        let entries = parser
            .parse(&fixture_path)
            .expect("Failed to parse package-lock.json fixture");

        // Should have workspace root + workspace member + external dependencies
        assert!(
            entries.len() >= 5,
            "Expected at least 5 entries, got {}",
            entries.len()
        );

        // Check workspace root
        let workspace_root = entries
            .iter()
            .find(|e| e.name == "test-workspace" && e.is_workspace_member)
            .expect("Workspace root not found");
        assert_eq!(workspace_root.version, "1.0.0");
        assert!(matches!(
            workspace_root.source,
            DependencySource::Workspace(_)
        ));
        assert!(workspace_root.dependencies.len() >= 1);

        // Check workspace member
        let app = entries
            .iter()
            .find(|e| e.name == "@test/app")
            .expect("@test/app workspace member not found");
        assert!(app.is_workspace_member);
        assert_eq!(app.version, "0.1.0");
        assert!(matches!(app.source, DependencySource::Workspace(_)));

        // Check external dependency
        let lodash = entries
            .iter()
            .find(|e| e.name == "lodash")
            .expect("lodash dependency not found");
        assert!(!lodash.is_workspace_member);
        assert_eq!(lodash.version, "4.17.21");
        assert!(matches!(lodash.source, DependencySource::Registry(_)));
        assert!(lodash.checksum.is_some());

        // Check dependency with its own dependencies
        let react = entries
            .iter()
            .find(|e| e.name == "react")
            .expect("react dependency not found");
        assert!(!react.is_workspace_member);
        assert_eq!(react.version, "18.2.0");
        assert!(react.dependencies.len() >= 1);
        assert_eq!(react.dependencies[0].name, "loose-envify");
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
    fn parses_bun_lockfile_fixture() {
        let fixture_path = Path::new(FIXTURES_DIR).join("bun.lock");
        let parser = BunLockfileParser;

        let entries = parser
            .parse(&fixture_path)
            .expect("Failed to parse bun.lock fixture");

        // Should have 2 workspace members + 3 packages
        assert!(
            entries.len() >= 5,
            "Expected at least 5 entries, got {}",
            entries.len()
        );

        // Check workspace root
        let workspace_root = entries
            .iter()
            .find(|e| e.name == "test-workspace" && e.is_workspace_member)
            .expect("Workspace root not found");
        assert_eq!(workspace_root.version, "1.0.0");
        assert!(matches!(
            workspace_root.source,
            DependencySource::Workspace(_)
        ));

        // Check workspace member
        let app = entries
            .iter()
            .find(|e| e.name == "@test/app")
            .expect("@test/app workspace member not found");
        assert!(app.is_workspace_member);
        assert_eq!(app.version, "0.1.0");

        // Check external dependency
        let lodash = entries
            .iter()
            .find(|e| e.name == "lodash")
            .expect("lodash dependency not found");
        assert!(!lodash.is_workspace_member);
        assert_eq!(lodash.version, "4.17.21");
        assert!(matches!(lodash.source, DependencySource::Registry(_)));
        assert!(lodash.checksum.is_some());

        // Check dependency with dependencies
        let react = entries
            .iter()
            .find(|e| e.name == "react")
            .expect("react dependency not found");
        assert!(!react.is_workspace_member);
        assert_eq!(react.version, "18.2.0");
        assert!(react.dependencies.len() >= 1);
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
    fn parses_pnpm_lockfile_fixture() {
        let fixture_path = Path::new(FIXTURES_DIR).join("pnpm-lock.yaml");
        let parser = PnpmLockfileParser;

        let entries = parser
            .parse(&fixture_path)
            .expect("Failed to parse pnpm-lock.yaml fixture");

        // Should have 2 workspace importers + 4 packages
        assert!(
            entries.len() >= 6,
            "Expected at least 6 entries, got {}",
            entries.len()
        );

        // Check workspace root
        let workspace_entries: Vec<_> = entries.iter().filter(|e| e.is_workspace_member).collect();
        assert!(
            workspace_entries.len() >= 2,
            "Expected at least 2 workspace members"
        );

        // Check external dependency
        let lodash = entries
            .iter()
            .find(|e| e.name == "lodash")
            .expect("lodash dependency not found");
        assert!(!lodash.is_workspace_member);
        assert_eq!(lodash.version, "4.17.21");
        assert!(matches!(lodash.source, DependencySource::Registry(_)));
        assert!(lodash.checksum.is_some());

        // Check dependency with dependencies
        let react = entries
            .iter()
            .find(|e| e.name == "react")
            .expect("react dependency not found");
        assert!(!react.is_workspace_member);
        assert_eq!(react.version, "18.2.0");
        assert!(react.dependencies.len() >= 1);
        assert_eq!(react.dependencies[0].name, "loose-envify");
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
    fn parses_yarn_classic_lockfile_fixture() {
        let fixture_path = Path::new(FIXTURES_DIR).join("yarn-classic.lock");
        let parser = YarnClassicLockfileParser;

        let entries = parser
            .parse(&fixture_path)
            .expect("Failed to parse yarn-classic.lock fixture");

        // Should have 4 packages
        assert!(
            entries.len() >= 4,
            "Expected at least 4 entries, got {}",
            entries.len()
        );

        // Check external dependency
        let lodash = entries
            .iter()
            .find(|e| e.name == "lodash")
            .expect("lodash dependency not found");
        assert!(!lodash.is_workspace_member);
        assert_eq!(lodash.version, "4.17.21");
        assert!(matches!(lodash.source, DependencySource::Registry(_)));
        assert!(lodash.checksum.is_some());

        // Check dependency with dependencies
        let react = entries
            .iter()
            .find(|e| e.name == "react")
            .expect("react dependency not found");
        assert!(!react.is_workspace_member);
        assert_eq!(react.version, "18.2.0");
        assert!(react.dependencies.len() >= 1);
        assert_eq!(react.dependencies[0].name, "loose-envify");
    }

    #[test]
    fn yarn_classic_parser_supports_correct_filename() {
        let parser = YarnClassicLockfileParser;
        assert!(parser.supports_lockfile(Path::new("yarn.lock")));
        assert!(!parser.supports_lockfile(Path::new("package-lock.json")));
        assert_eq!(parser.lockfile_name(), "yarn.lock");
    }

    #[test]
    fn yarn_classic_parser_detects_v1_lockfile_via_content_sniffing() {
        use std::fs;
        use std::io::Write;
        use tempfile::TempDir;

        // Create a unique temp directory with a yarn.lock file containing v1 content
        let temp_dir = TempDir::new().unwrap();
        let yarn_lock_path = temp_dir.path().join("yarn.lock");

        let classic_content =
            fs::read_to_string(Path::new(FIXTURES_DIR).join("yarn-classic.lock")).unwrap();
        fs::write(&yarn_lock_path, classic_content).unwrap();

        let parser = YarnClassicLockfileParser;

        // Yarn Classic parser should accept v1 lockfile based on content
        assert!(
            parser.supports_lockfile(&yarn_lock_path),
            "Yarn Classic parser should accept v1 lockfile"
        );
    }

    #[test]
    fn yarn_classic_parser_rejects_modern_lockfile_via_content_sniffing() {
        use std::fs;
        use tempfile::TempDir;

        // Create a unique temp directory with a yarn.lock file containing v2 content
        let temp_dir = TempDir::new().unwrap();
        let yarn_lock_path = temp_dir.path().join("yarn.lock");

        let modern_content =
            fs::read_to_string(Path::new(FIXTURES_DIR).join("yarn-modern.lock")).unwrap();
        fs::write(&yarn_lock_path, modern_content).unwrap();

        let parser = YarnClassicLockfileParser;

        // Yarn Classic parser should reject v2+ lockfile based on content
        assert!(
            !parser.supports_lockfile(&yarn_lock_path),
            "Yarn Classic parser should reject v2+ lockfile with __metadata"
        );
    }
}

#[cfg(feature = "parser-yarn-modern")]
mod yarn_modern_tests {
    use super::*;
    use cuenv_workspaces::YarnModernLockfileParser;

    #[test]
    fn parses_yarn_modern_lockfile_fixture() {
        let fixture_path = Path::new(FIXTURES_DIR).join("yarn-modern.lock");
        let parser = YarnModernLockfileParser;

        let entries = parser
            .parse(&fixture_path)
            .expect("Failed to parse yarn-modern.lock fixture");

        // Should have entries for packages and workspace members
        assert!(
            entries.len() >= 5,
            "Expected at least 5 entries, got {}",
            entries.len()
        );

        // Check workspace root
        let workspace_root = entries
            .iter()
            .find(|e| e.name == "test-workspace" && e.is_workspace_member)
            .expect("Workspace root not found");
        assert!(matches!(
            workspace_root.source,
            DependencySource::Workspace(_)
        ));

        // Check workspace member
        let app = entries
            .iter()
            .find(|e| e.name == "@test/app")
            .expect("@test/app workspace member not found");
        assert!(app.is_workspace_member);
        assert!(matches!(app.source, DependencySource::Workspace(_)));

        // Check external dependency
        let lodash = entries
            .iter()
            .find(|e| e.name == "lodash")
            .expect("lodash dependency not found");
        assert!(!lodash.is_workspace_member);
        assert_eq!(lodash.version, "4.17.21");
        assert!(matches!(lodash.source, DependencySource::Registry(_)));
        assert!(lodash.checksum.is_some());

        // Check dependency with dependencies
        let react = entries
            .iter()
            .find(|e| e.name == "react")
            .expect("react dependency not found");
        assert!(!react.is_workspace_member);
        assert_eq!(react.version, "18.2.0");
        assert!(react.dependencies.len() >= 1);
    }

    #[test]
    fn yarn_modern_parser_supports_correct_filename() {
        let parser = YarnModernLockfileParser;
        assert!(parser.supports_lockfile(Path::new("yarn.lock")));
        assert!(!parser.supports_lockfile(Path::new("package-lock.json")));
        assert_eq!(parser.lockfile_name(), "yarn.lock");
    }

    #[test]
    fn yarn_modern_parser_detects_v2_lockfile_via_content_sniffing() {
        use std::fs;
        use tempfile::TempDir;

        // Create a unique temp directory with a yarn.lock file containing v2 content
        let temp_dir = TempDir::new().unwrap();
        let yarn_lock_path = temp_dir.path().join("yarn.lock");

        let modern_content =
            fs::read_to_string(Path::new(FIXTURES_DIR).join("yarn-modern.lock")).unwrap();
        fs::write(&yarn_lock_path, modern_content).unwrap();

        let parser = YarnModernLockfileParser;

        // Yarn Modern parser should accept v2+ lockfile based on content
        assert!(
            parser.supports_lockfile(&yarn_lock_path),
            "Yarn Modern parser should accept v2+ lockfile with __metadata"
        );
    }

    #[test]
    fn yarn_modern_parser_rejects_classic_lockfile_via_content_sniffing() {
        use std::fs;
        use tempfile::TempDir;

        // Create a unique temp directory with a yarn.lock file containing v1 content
        let temp_dir = TempDir::new().unwrap();
        let yarn_lock_path = temp_dir.path().join("yarn.lock");

        let classic_content =
            fs::read_to_string(Path::new(FIXTURES_DIR).join("yarn-classic.lock")).unwrap();
        fs::write(&yarn_lock_path, classic_content).unwrap();

        let parser = YarnModernLockfileParser;

        // Yarn Modern parser should reject v1 lockfile based on content
        assert!(
            !parser.supports_lockfile(&yarn_lock_path),
            "Yarn Modern parser should reject v1 lockfile"
        );
    }
}
