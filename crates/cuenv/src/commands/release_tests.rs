use super::*;
use std::fs;
use std::process::Command;
use tempfile::TempDir;

/// Create a test directory with proper prefix (non-hidden) for gix compatibility.
///
/// gix has stricter checks on temp directories that start with `.` (hidden directories).
/// The default `TempDir::new()` creates hidden directories like `.tmpXXXXX`, which can
/// cause gix to fail with "does not appear to be a git repository".
fn create_test_dir() -> TempDir {
    tempfile::Builder::new()
        .prefix("cuenv_test_")
        .tempdir()
        .expect("Failed to create temp directory")
}

fn create_test_workspace(temp: &TempDir) -> String {
    let root = temp.path();

    // Create root Cargo.toml
    let root_manifest = r#"[workspace]
resolver = "2"
members = ["crates/foo", "crates/bar"]

[workspace.package]
version = "1.0.0"
edition = "2021"

[workspace.dependencies]
foo = { path = "crates/foo", version = "1.0.0" }
bar = { path = "crates/bar", version = "1.0.0" }
"#;
    fs::write(root.join("Cargo.toml"), root_manifest).unwrap();

    // Create member crates
    fs::create_dir_all(root.join("crates/foo")).unwrap();
    fs::create_dir_all(root.join("crates/bar")).unwrap();

    let foo_manifest = r#"[package]
name = "foo"
version.workspace = true
"#;
    fs::write(root.join("crates/foo/Cargo.toml"), foo_manifest).unwrap();

    let bar_manifest = r#"[package]
name = "bar"
version.workspace = true
"#;
    fs::write(root.join("crates/bar/Cargo.toml"), bar_manifest).unwrap();

    root.to_string_lossy().to_string()
}

#[test]
fn test_changeset_add() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().to_str().unwrap();

    let packages = vec![("my-pkg".to_string(), "minor".to_string())];

    let result = execute_changeset_add(path, &packages, Some("Add feature"), Some("Details here"));

    assert!(result.is_ok());
    let output = result.unwrap();
    assert!(output.contains("Created changeset"));
    assert!(output.contains("Add feature"));
}

#[test]
fn test_changeset_add_invalid_bump() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().to_str().unwrap();

    let packages = vec![("my-pkg".to_string(), "invalid".to_string())];

    let result = execute_changeset_add(path, &packages, Some("Test"), None);
    assert!(result.is_err());
}

#[test]
fn test_changeset_add_no_packages() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().to_str().unwrap();

    let packages: Vec<(String, String)> = vec![];

    let result = execute_changeset_add(path, &packages, Some("Test"), None);
    assert!(result.is_err());
}

#[test]
fn test_changeset_status_empty() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().to_str().unwrap();

    let result = execute_changeset_status(path);
    assert!(result.is_ok());
    assert!(result.unwrap().contains("No pending changesets"));
}

#[test]
fn test_changeset_status_with_changesets() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().to_str().unwrap();

    // First add a changeset
    let packages = vec![("pkg-a".to_string(), "minor".to_string())];
    execute_changeset_add(path, &packages, Some("Add feature"), None).unwrap();

    // Then check status
    let result = execute_changeset_status(path);
    assert!(result.is_ok());
    let output = result.unwrap();
    assert!(output.contains("1 pending changeset"));
    assert!(output.contains("Add feature"));
    assert!(output.contains("pkg-a"));
}

#[test]
fn test_release_version_no_changesets() {
    let temp = TempDir::new().unwrap();
    let path = create_test_workspace(&temp);

    let result = execute_release_version(&path, true.into());
    assert!(result.is_err());
}

#[test]
fn test_release_version_dry_run() {
    let temp = TempDir::new().unwrap();
    let path = create_test_workspace(&temp);

    // Add a changeset first
    let packages = vec![("foo".to_string(), "minor".to_string())];
    execute_changeset_add(&path, &packages, Some("Feature"), None).unwrap();

    let result = execute_release_version(&path, true.into());
    assert!(result.is_ok());
    let output = result.unwrap();
    assert!(output.contains("Dry run"));
    assert!(output.contains("Version changes"));
}

#[test]
fn test_release_version_apply() {
    let temp = TempDir::new().unwrap();
    let path = create_test_workspace(&temp);

    // Add a changeset
    let packages = vec![("foo".to_string(), "minor".to_string())];
    execute_changeset_add(&path, &packages, Some("Feature"), None).unwrap();

    // Apply version changes
    let result = execute_release_version(&path, false.into());
    assert!(result.is_ok());
    let output = result.unwrap();
    assert!(output.contains("Manifest files updated"));
    assert!(output.contains("Changesets have been consumed"));

    // Verify version was updated
    let manifest = CargoManifest::new(Path::new(&path));
    let version = manifest.read_workspace_version().unwrap();
    assert_eq!(version.to_string(), "1.1.0");
}

#[test]
fn test_release_publish_dry_run_human() {
    let temp = TempDir::new().unwrap();
    let path = create_test_workspace(&temp);

    let result = execute_release_publish(&path, true.into(), OutputFormat::Human);
    assert!(result.is_ok());
    let output = result.unwrap();
    assert!(output.contains("Dry run"));
    assert!(output.contains("Publish plan"));
}

#[test]
fn test_release_publish_json() {
    let temp = TempDir::new().unwrap();
    let path = create_test_workspace(&temp);

    let result = execute_release_publish(&path, true.into(), OutputFormat::Json);
    assert!(result.is_ok());
    let output = result.unwrap();
    assert!(output.contains("\"packages\""));
    assert!(output.contains("bar"));
    assert!(output.contains("foo"));
}

/// Helper function to initialize and configure a git repository for testing
fn init_git_repo(path: &str) {
    let out = Command::new("git")
        .args(["init"])
        .current_dir(path)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git init failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Verify .git directory and HEAD file were created (ensures git init fully completed)
    let git_dir = std::path::Path::new(path).join(".git");
    let git_head = git_dir.join("HEAD");
    assert!(
        git_dir.exists(),
        "git init did not create .git directory at {}",
        git_dir.display()
    );
    assert!(
        git_head.exists(),
        "git init did not create .git/HEAD at {}",
        git_head.display()
    );

    let out = Command::new("git")
        .args(["config", "user.name", "Test User"])
        .current_dir(path)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git config user.name failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let out = Command::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(path)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git config user.email failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Helper function to create a git commit
fn create_git_commit(path: &str, message: &str) {
    let out = Command::new("git")
        .args(["add", "."])
        .current_dir(path)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git add failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let out = Command::new("git")
        .args(["commit", "--no-gpg-sign", "-m", message])
        .current_dir(path)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git commit failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn test_changeset_from_commits_no_git_repo() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().to_str().unwrap();

    // Should fail because there's no git repository
    let result = execute_changeset_from_commits(path, None);
    assert!(result.is_err());
}

#[test]
fn test_changeset_from_commits_with_workspace() {
    let temp = create_test_dir();
    let path = create_test_workspace(&temp);

    init_git_repo(&path);
    create_git_commit(&path, "feat: add new feature");

    // Now test the function
    let result = execute_changeset_from_commits(&path, None);
    assert!(result.is_ok(), "Expected Ok, got error: {:?}", result.err());
    let output = result.unwrap();
    assert!(output.contains("Created changeset"));
    assert!(output.contains("conventional commit"));
}

#[test]
fn test_changeset_from_commits_no_version_bumps() {
    let temp = create_test_dir();
    let path = create_test_workspace(&temp);

    init_git_repo(&path);
    create_git_commit(&path, "chore: update deps");

    // Should return message about no version-bumping commits
    let result = execute_changeset_from_commits(&path, None);
    assert!(result.is_ok(), "Expected Ok, got error: {:?}", result.err());
    let output = result.unwrap();
    assert!(output.contains("No version-bumping commits"));
}

#[test]
fn test_changeset_from_commits_with_since_tag() {
    let temp = create_test_dir();
    let path = create_test_workspace(&temp);

    init_git_repo(&path);
    create_git_commit(&path, "fix: initial fix");

    // Create a tag (use -m to create annotated tag, works with all git configs)
    let out = std::process::Command::new("git")
        .args(["tag", "--no-sign", "-m", "Release v0.1.0", "v0.1.0"])
        .current_dir(&path)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git tag failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Verify tag was created
    let out = std::process::Command::new("git")
        .args(["tag", "-l", "v0.1.0"])
        .current_dir(&path)
        .output()
        .unwrap();
    assert!(
        out.status.success() && String::from_utf8_lossy(&out.stdout).trim() == "v0.1.0",
        "git tag verification failed: stdout={}, stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    // Create a second commit (after tag) - this should be picked up
    // The file must be in a package directory for per-package analysis to detect it
    let new_file = std::path::Path::new(&path).join("crates/foo/new-feature.rs");
    std::fs::write(new_file, "// new feature").unwrap();
    create_git_commit(&path, "feat: new feature after tag");

    // Test with since_tag - should only process commits after the tag
    let result = execute_changeset_from_commits(&path, Some("v0.1.0"));
    assert!(result.is_ok(), "Expected Ok, got error: {:?}", result.err());
    let output = result.unwrap();
    assert!(output.contains("Created changeset"));
    assert!(output.contains("conventional commit"));
    // Should have created changeset from 1 commit (the one after the tag)
    assert!(output.contains("1 conventional commit"));
    // Only foo should be affected (not bar)
    assert!(output.contains("foo"));
}

#[test]
fn test_changeset_from_commits_with_nonexistent_tag() {
    let temp = create_test_dir();
    let path = create_test_workspace(&temp);

    init_git_repo(&path);
    create_git_commit(&path, "feat: new feature");

    // Test with non-existent tag - should return error
    let result = execute_changeset_from_commits(&path, Some("v0.1.0"));
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.to_string().contains("Tag 'v0.1.0' not found"));
}
