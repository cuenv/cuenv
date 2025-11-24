#![cfg(feature = "parser-cargo")]

//! Integration tests for the Cargo lockfile parser.

use cuenv_workspaces::{CargoLockfileParser, DependencySource, LockfileEntry, LockfileParser};
use std::fs;
use std::io::Write;
use std::path::Path;
use tempfile::TempDir;

const REPO_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");

#[test]
fn test_parse_real_cargo_lock() {
    let parser = CargoLockfileParser;
    let entries = parse_repo_lockfile(parser);
    assert!(entries.len() > 100, "expected a sizable lockfile");
}

#[test]
fn test_identifies_cuenv_workspace_members() {
    let parser = CargoLockfileParser;
    let entries = parse_repo_lockfile(parser);
    let expected = ["cuenv-core", "cuenv-cli", "cuengine", "cuenv-workspaces"];

    for name in expected {
        let entry = entries
            .iter()
            .find(|e| e.name == name)
            .unwrap_or_else(|| panic!("missing entry for {name}"));
        assert!(
            entry.is_workspace_member,
            "{name} should be a workspace member"
        );
        assert!(matches!(entry.source, DependencySource::Workspace(_)));
    }
}

#[test]
fn test_external_dependencies_are_registry() {
    let parser = CargoLockfileParser;
    let entries = parse_repo_lockfile(parser);
    let expected = ["serde", "tokio"];

    for name in expected {
        let entry = entries
            .iter()
            .find(|e| e.name == name)
            .unwrap_or_else(|| panic!("missing entry for {name}"));
        assert!(!entry.is_workspace_member);
        assert!(matches!(entry.source, DependencySource::Registry(_)));
    }
}

#[test]
fn test_workspace_member_dependencies() {
    let parser = CargoLockfileParser;
    let entries = parse_repo_lockfile(parser);
    let cli = entries
        .iter()
        .find(|e| e.name == "cuenv-cli")
        .expect("missing cuenv-cli");

    let dep_names: Vec<_> = cli.dependencies.iter().map(|d| d.name.as_str()).collect();
    assert!(dep_names.contains(&"cuenv-core"));
    assert!(dep_names.contains(&"cuengine"));
}

#[test]
fn test_handles_missing_cargo_toml() {
    let temp = TempDir::new().unwrap();
    write_cargo_lock(temp.path(), "version = 4\n");

    let parser = CargoLockfileParser;
    let err = parser
        .parse(&temp.path().join("Cargo.lock"))
        .expect_err("expected failure");
    match err {
        cuenv_workspaces::Error::ManifestNotFound { path } => {
            assert!(path.ends_with("Cargo.toml"));
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn test_handles_invalid_cargo_lock() {
    let temp = create_test_workspace(&["crates/app"]);
    write_cargo_lock(temp.path(), "this is not valid toml");
    let parser = CargoLockfileParser;
    let err = parser
        .parse(&temp.path().join("Cargo.lock"))
        .expect_err("expected failure");
    match err {
        cuenv_workspaces::Error::LockfileParseFailed { .. } => {}
        other => panic!("unexpected error: {other:?}"),
    }
}

fn parse_repo_lockfile(parser: CargoLockfileParser) -> Vec<LockfileEntry> {
    let lock_path = Path::new(REPO_ROOT).join("Cargo.lock");
    parser
        .parse(&lock_path)
        .expect("failed to parse repo Cargo.lock")
}

fn create_test_workspace(members: &[&str]) -> TempDir {
    let temp = TempDir::new().unwrap();
    let mut manifest = String::from("[workspace]\n");
    manifest.push_str("members = [\n");
    for member in members {
        manifest.push_str("    \"");
        manifest.push_str(member);
        manifest.push_str("\",\n");
    }
    manifest.push_str("]\n");
    write_cargo_toml(temp.path(), &manifest);

    for member in members {
        let path = temp.path().join(member);
        fs::create_dir_all(&path).unwrap();
        let name = Path::new(member).file_name().map_or_else(
            || member.replace('/', "-"),
            |n| n.to_string_lossy().to_string(),
        );
        let member_manifest = format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n");
        fs::write(path.join("Cargo.toml"), member_manifest).unwrap();
    }

    temp
}

fn write_cargo_toml(dir: &Path, content: &str) {
    fs::create_dir_all(dir).unwrap();
    fs::write(dir.join("Cargo.toml"), content).unwrap();
}

fn write_cargo_lock(dir: &Path, content: &str) {
    let mut file = fs::File::create(dir.join("Cargo.lock")).unwrap();
    file.write_all(content.as_bytes()).unwrap();
}
