//! Integration tests for the Cargo lockfile parser.

#![cfg(feature = "parser-cargo")]

use cuenv_workspaces::{CargoLockfileParser, DependencySource, LockfileEntry, LockfileParser};
use std::error::Error;
use std::fs;
use std::io::Write;
use std::path::Path;
use tempfile::TempDir;

const REPO_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

#[test]
fn test_parse_real_cargo_lock() -> TestResult {
    let parser = CargoLockfileParser;
    let entries = parse_repo_lockfile(parser)?;
    assert!(entries.len() > 100, "expected a sizable lockfile");
    Ok(())
}

#[test]
fn test_identifies_cuenv_workspace_members() -> TestResult {
    let parser = CargoLockfileParser;
    let entries = parse_repo_lockfile(parser)?;
    let expected = ["cuenv-core", "cuenv", "cuengine", "cuenv-workspaces"];

    for name in expected {
        let entry = entry_named(&entries, name)?;
        assert!(
            entry.is_workspace_member,
            "{name} should be a workspace member"
        );
        assert!(matches!(entry.source, DependencySource::Workspace(_)));
    }
    Ok(())
}

#[test]
fn test_external_dependencies_are_registry() -> TestResult {
    let parser = CargoLockfileParser;
    let entries = parse_repo_lockfile(parser)?;
    let expected = ["serde", "tokio"];

    for name in expected {
        let entry = entry_named(&entries, name)?;
        assert!(!entry.is_workspace_member);
        assert!(matches!(entry.source, DependencySource::Registry(_)));
    }
    Ok(())
}

#[test]
fn test_workspace_member_dependencies() -> TestResult {
    let parser = CargoLockfileParser;
    let entries = parse_repo_lockfile(parser)?;
    let cli = entry_named(&entries, "cuenv")?;

    let dep_names: Vec<_> = cli.dependencies.iter().map(|d| d.name.as_str()).collect();
    assert!(dep_names.contains(&"cuenv-core"));
    assert!(dep_names.contains(&"cuengine"));
    Ok(())
}

#[test]
fn test_handles_missing_cargo_toml() -> TestResult {
    let temp = TempDir::new()?;
    write_cargo_lock(temp.path(), "version = 4\n")?;

    let parser = CargoLockfileParser;
    let err = parse_should_fail(parser, &temp.path().join("Cargo.lock"))?;
    match err {
        cuenv_workspaces::Error::ManifestNotFound { path } => {
            assert!(path.ends_with("Cargo.toml"));
        }
        other => return Err(unexpected_error(&other).into()),
    }
    Ok(())
}

#[test]
fn test_handles_invalid_cargo_lock() -> TestResult {
    let temp = create_test_workspace(&["crates/app"])?;
    write_cargo_lock(temp.path(), "this is not valid toml")?;
    let parser = CargoLockfileParser;
    let err = parse_should_fail(parser, &temp.path().join("Cargo.lock"))?;
    match err {
        cuenv_workspaces::Error::LockfileParseFailed { .. } => {}
        other => return Err(unexpected_error(&other).into()),
    }
    Ok(())
}

fn parse_repo_lockfile(parser: CargoLockfileParser) -> TestResult<Vec<LockfileEntry>> {
    let lock_path = Path::new(REPO_ROOT).join("Cargo.lock");
    Ok(parser.parse(&lock_path)?)
}

fn entry_named<'a>(entries: &'a [LockfileEntry], name: &str) -> TestResult<&'a LockfileEntry> {
    entries
        .iter()
        .find(|entry| entry.name == name)
        .ok_or_else(|| missing_entry(name).into())
}

fn parse_should_fail(
    parser: CargoLockfileParser,
    lock_path: &Path,
) -> TestResult<cuenv_workspaces::Error> {
    match parser.parse(lock_path) {
        Ok(entries) => Err(std::io::Error::other(format!(
            "expected Cargo lockfile parsing to fail, parsed {} entries",
            entries.len()
        ))
        .into()),
        Err(err) => Ok(err),
    }
}

fn missing_entry(name: &str) -> std::io::Error {
    std::io::Error::other(format!("missing entry for {name}"))
}

fn unexpected_error(error: &cuenv_workspaces::Error) -> std::io::Error {
    std::io::Error::other(format!("unexpected error: {error:?}"))
}

fn create_test_workspace(members: &[&str]) -> TestResult<TempDir> {
    let temp = TempDir::new()?;
    let mut manifest = String::from("[workspace]\n");
    manifest.push_str("members = [\n");
    for member in members {
        manifest.push_str("    \"");
        manifest.push_str(member);
        manifest.push_str("\",\n");
    }
    manifest.push_str("]\n");
    write_cargo_toml(temp.path(), &manifest)?;

    for member in members {
        let path = temp.path().join(member);
        fs::create_dir_all(&path)?;
        let name = Path::new(member).file_name().map_or_else(
            || member.replace('/', "-"),
            |n| n.to_string_lossy().to_string(),
        );
        let member_manifest = format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n");
        fs::write(path.join("Cargo.toml"), member_manifest)?;
    }

    Ok(temp)
}

fn write_cargo_toml(dir: &Path, content: &str) -> TestResult {
    fs::create_dir_all(dir)?;
    fs::write(dir.join("Cargo.toml"), content)?;
    Ok(())
}

fn write_cargo_lock(dir: &Path, content: &str) -> TestResult {
    let mut file = fs::File::create(dir.join("Cargo.lock"))?;
    file.write_all(content.as_bytes())?;
    Ok(())
}
