#![allow(missing_docs)]
use cuenv_workspaces::WorkspaceDiscovery;
use std::path::{Path, PathBuf};

#[cfg(feature = "discovery-javascript")]
use cuenv_workspaces::{PackageJsonDiscovery, PnpmWorkspaceDiscovery};

#[cfg(feature = "discovery-rust")]
use cuenv_workspaces::CargoTomlDiscovery;

// Helper to find fixtures directory
fn get_fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
}

#[cfg(feature = "discovery-javascript")]
#[test]
fn test_package_json_discovery() {
    let root = get_fixtures_dir().join("test-workspace-npm");
    let discovery = PackageJsonDiscovery;

    let workspace = discovery
        .discover(&root)
        .expect("Should discover workspace");

    assert_eq!(workspace.root, root);
    assert_eq!(workspace.member_count(), 2); // pkg-a, pkg-b. excluded is excluded.

    // Check members
    let pkg_a = workspace.find_member("pkg-a").expect("Should find pkg-a");
    assert_eq!(pkg_a.name, "pkg-a");
    assert!(pkg_a.dependencies.contains(&"pkg-b".to_string()));
    assert!(pkg_a.dependencies.contains(&"lodash".to_string()));

    let pkg_b = workspace.find_member("pkg-b").expect("Should find pkg-b");
    assert_eq!(pkg_b.name, "pkg-b");
    assert!(pkg_b.dependencies.contains(&"react".to_string()));
    assert!(pkg_b.dependencies.contains(&"typescript".to_string()));
}

#[cfg(feature = "discovery-javascript")]
#[test]
fn test_pnpm_workspace_discovery() {
    let root = get_fixtures_dir().join("test-workspace-pnpm");
    let discovery = PnpmWorkspaceDiscovery;

    let workspace = discovery
        .discover(&root)
        .expect("Should discover workspace");

    assert_eq!(workspace.root, root);
    // tool-a is the only one we created
    assert_eq!(workspace.member_count(), 1);

    let tool_a = workspace.find_member("tool-a").expect("Should find tool-a");
    assert_eq!(tool_a.name, "tool-a");
    assert!(tool_a.dependencies.contains(&"tool-b".to_string()));
    assert!(tool_a.dependencies.contains(&"react".to_string()));
}

#[cfg(feature = "discovery-rust")]
#[test]
fn test_cargo_workspace_discovery() {
    let root = get_fixtures_dir().join("test-workspace-cargo");
    let discovery = CargoTomlDiscovery;

    let workspace = discovery
        .discover(&root)
        .expect("Should discover workspace");

    assert_eq!(workspace.root, root);
    assert_eq!(workspace.member_count(), 2); // lib-a, lib-b

    let lib_a = workspace.find_member("lib-a").expect("Should find lib-a");
    assert_eq!(lib_a.name, "lib-a");
    assert!(lib_a.dependencies.contains(&"lib-b".to_string()));
    assert!(lib_a.dependencies.contains(&"serde".to_string()));
    assert!(lib_a.dependencies.contains(&"tokio".to_string()));

    let lib_b = workspace.find_member("lib-b").expect("Should find lib-b");
    assert_eq!(lib_b.name, "lib-b");
    assert!(lib_b.dependencies.contains(&"thiserror".to_string()));
}

#[test]
#[cfg(feature = "discovery-javascript")]
fn test_package_json_missing_workspace() {
    let discovery = PackageJsonDiscovery;
    let result = discovery.discover(Path::new("/nonexistent/path"));
    assert!(result.is_err());
}

// --- New Tests for Edge Cases ---

#[cfg(feature = "discovery-javascript")]
#[test]
fn test_package_json_object_style_workspaces() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();

    // Create package.json with object-style workspaces
    let package_json = r#"{
        "name": "root",
        "workspaces": {
            "packages": ["packages/*"]
        }
    }"#;
    std::fs::write(root.join("package.json"), package_json).unwrap();

    // Create member package
    let pkg_path = root.join("packages").join("pkg-a");
    std::fs::create_dir_all(&pkg_path).unwrap();
    std::fs::write(pkg_path.join("package.json"), r#"{"name": "pkg-a"}"#).unwrap();

    let discovery = PackageJsonDiscovery;
    let workspace = discovery
        .discover(root)
        .expect("Should discover workspace with object-style config");

    assert_eq!(workspace.member_count(), 1);
    assert!(workspace.find_member("pkg-a").is_some());
}

#[cfg(feature = "discovery-javascript")]
#[test]
fn test_package_json_member_missing_manifest_ignored() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();

    // Create root package.json
    let package_json = r#"{
        "name": "root",
        "workspaces": ["packages/*"]
    }"#;
    std::fs::write(root.join("package.json"), package_json).unwrap();

    // Create directory that matches glob but has no package.json
    let pkg_path = root.join("packages").join("missing-json");
    std::fs::create_dir_all(&pkg_path).unwrap();

    let discovery = PackageJsonDiscovery;
    let workspace = discovery
        .discover(root)
        .expect("Should discover workspace and ignore invalid members");

    // Should succeed but have 0 members
    assert_eq!(workspace.member_count(), 0);
}

#[cfg(feature = "discovery-javascript")]
#[test]
fn test_package_json_member_malformed_manifest_ignored() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();

    // Create root package.json
    let package_json = r#"{
        "name": "root",
        "workspaces": ["packages/*"]
    }"#;
    std::fs::write(root.join("package.json"), package_json).unwrap();

    // Create member with malformed package.json
    let pkg_path = root.join("packages").join("malformed");
    std::fs::create_dir_all(&pkg_path).unwrap();
    std::fs::write(pkg_path.join("package.json"), "{ invalid json").unwrap();

    let discovery = PackageJsonDiscovery;
    let workspace = discovery
        .discover(root)
        .expect("Should discover workspace and ignore invalid members");

    // Should succeed but have 0 members
    assert_eq!(workspace.member_count(), 0);
}

#[cfg(feature = "discovery-javascript")]
#[test]
fn test_pnpm_workspace_malformed_yaml() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();

    // Create malformed pnpm-workspace.yaml
    std::fs::write(
        root.join("pnpm-workspace.yaml"),
        "packages: [unclosed bracket",
    )
    .unwrap();

    let discovery = PnpmWorkspaceDiscovery;
    let result = discovery.discover(root);

    assert!(result.is_err());
    match result.unwrap_err() {
        cuenv_workspaces::Error::Yaml { .. } => {}
        e => panic!("Expected Yaml error, got {:?}", e),
    }
}

#[cfg(feature = "discovery-javascript")]
#[test]
fn test_pnpm_workspace_exclusions() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();

    // Create pnpm-workspace.yaml with exclusions
    let workspace_yaml = r#"
    packages:
      - 'packages/*'
      - '!packages/excluded'
    "#;
    std::fs::write(root.join("pnpm-workspace.yaml"), workspace_yaml).unwrap();

    // Create included package
    let pkg_a = root.join("packages").join("pkg-a");
    std::fs::create_dir_all(&pkg_a).unwrap();
    std::fs::write(pkg_a.join("package.json"), r#"{"name": "pkg-a"}"#).unwrap();

    // Create excluded package
    let excluded = root.join("packages").join("excluded");
    std::fs::create_dir_all(&excluded).unwrap();
    std::fs::write(excluded.join("package.json"), r#"{"name": "excluded"}"#).unwrap();

    let discovery = PnpmWorkspaceDiscovery;
    let workspace = discovery.discover(root).expect("Should discover workspace");

    assert_eq!(workspace.member_count(), 1);
    assert!(workspace.find_member("pkg-a").is_some());
    assert!(workspace.find_member("excluded").is_none());
}

#[cfg(feature = "discovery-rust")]
#[test]
fn test_cargo_toml_missing_workspace_section_returns_empty() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();

    // Create Cargo.toml without [workspace] (single-package repository)
    let cargo_toml = r#"
    [package]
    name = "root"
    version = "0.1.0"
    "#;
    std::fs::write(root.join("Cargo.toml"), cargo_toml).unwrap();

    let discovery = CargoTomlDiscovery;
    let workspace = discovery
        .discover(root)
        .expect("Should discover workspace even without [workspace] section");

    // Should be an empty workspace (single-package repo has zero workspace members)
    assert_eq!(workspace.member_count(), 0);
    assert_eq!(workspace.manager, cuenv_workspaces::PackageManager::Cargo);
    assert_eq!(workspace.root, root);
}

#[cfg(feature = "discovery-rust")]
#[test]
fn test_cargo_toml_missing_workspace_section_find_members_returns_empty() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();

    // Create Cargo.toml without [workspace]
    let cargo_toml = r#"
    [package]
    name = "single-package"
    version = "0.1.0"
    "#;
    std::fs::write(root.join("Cargo.toml"), cargo_toml).unwrap();

    let discovery = CargoTomlDiscovery;
    let members = discovery
        .find_members(root)
        .expect("Should return empty members for single-package repo");

    // find_members should also return an empty vector
    assert_eq!(members.len(), 0);
}

#[cfg(feature = "discovery-rust")]
#[test]
fn test_cargo_toml_missing_workspace_section_with_lockfile() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();

    // Create Cargo.toml without [workspace]
    let cargo_toml = r#"
    [package]
    name = "root"
    version = "0.1.0"
    "#;
    std::fs::write(root.join("Cargo.toml"), cargo_toml).unwrap();
    std::fs::write(root.join("Cargo.lock"), "# Dummy lockfile").unwrap();

    let discovery = CargoTomlDiscovery;
    let workspace = discovery.discover(root).expect("Should discover workspace");

    // Lockfile should be detected even without [workspace] section
    assert!(workspace.lockfile.is_some());
    assert_eq!(workspace.lockfile.unwrap(), root.join("Cargo.lock"));
}

#[cfg(feature = "discovery-rust")]
#[test]
fn test_cargo_member_missing_package_section_ignored() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();

    // Create root Cargo.toml
    let cargo_toml = r#"
    [workspace]
    members = ["crates/*"]
    "#;
    std::fs::write(root.join("Cargo.toml"), cargo_toml).unwrap();

    // Create member with Cargo.toml missing [package]
    let member_path = root.join("crates").join("member");
    std::fs::create_dir_all(&member_path).unwrap();
    std::fs::write(member_path.join("Cargo.toml"), "").unwrap(); // Empty file

    let discovery = CargoTomlDiscovery;
    let workspace = discovery
        .discover(root)
        .expect("Should discover workspace and ignore invalid members");

    // Should succeed but have 0 members
    assert_eq!(workspace.member_count(), 0);
}

#[cfg(feature = "discovery-javascript")]
#[test]
fn test_nested_workspace_patterns() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();

    // Create package.json with nested patterns
    let package_json = r#"{
        "name": "root",
        "workspaces": ["packages/*/apps/*"]
    }"#;
    std::fs::write(root.join("package.json"), package_json).unwrap();

    // Structure: packages/group1/apps/app-a
    let app_path = root
        .join("packages")
        .join("group1")
        .join("apps")
        .join("app-a");
    std::fs::create_dir_all(&app_path).unwrap();
    std::fs::write(app_path.join("package.json"), r#"{"name": "app-a"}"#).unwrap();

    // Structure: packages/group2/apps/app-b
    let app_b_path = root
        .join("packages")
        .join("group2")
        .join("apps")
        .join("app-b");
    std::fs::create_dir_all(&app_b_path).unwrap();
    std::fs::write(app_b_path.join("package.json"), r#"{"name": "app-b"}"#).unwrap();

    // Structure: packages/group1/libs/lib-a (should not match)
    let lib_path = root
        .join("packages")
        .join("group1")
        .join("libs")
        .join("lib-a");
    std::fs::create_dir_all(&lib_path).unwrap();
    std::fs::write(lib_path.join("package.json"), r#"{"name": "lib-a"}"#).unwrap();

    let discovery = PackageJsonDiscovery;
    let workspace = discovery
        .discover(root)
        .expect("Should discover nested workspaces");

    assert_eq!(workspace.member_count(), 2);
    assert!(workspace.find_member("app-a").is_some());
    assert!(workspace.find_member("app-b").is_some());
    assert!(workspace.find_member("lib-a").is_none());
}

#[cfg(feature = "discovery-javascript")]
#[test]
fn test_pnpm_mixed_exclusions() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();

    // Create pnpm-workspace.yaml with mixed includes and excludes
    let workspace_yaml = r#"
    packages:
      - 'packages/*'
      - '!packages/exclude-me'
      - 'apps/*'
      - '!apps/test-*'
    "#;
    std::fs::write(root.join("pnpm-workspace.yaml"), workspace_yaml).unwrap();

    // packages/pkg-a (include)
    let pkg_a = root.join("packages").join("pkg-a");
    std::fs::create_dir_all(&pkg_a).unwrap();
    std::fs::write(pkg_a.join("package.json"), r#"{"name": "pkg-a"}"#).unwrap();

    // packages/exclude-me (exclude)
    let exclude_me = root.join("packages").join("exclude-me");
    std::fs::create_dir_all(&exclude_me).unwrap();
    std::fs::write(exclude_me.join("package.json"), r#"{"name": "exclude-me"}"#).unwrap();

    // apps/app-1 (include)
    let app_1 = root.join("apps").join("app-1");
    std::fs::create_dir_all(&app_1).unwrap();
    std::fs::write(app_1.join("package.json"), r#"{"name": "app-1"}"#).unwrap();

    // apps/test-app (exclude)
    let test_app = root.join("apps").join("test-app");
    std::fs::create_dir_all(&test_app).unwrap();
    std::fs::write(test_app.join("package.json"), r#"{"name": "test-app"}"#).unwrap();

    let discovery = PnpmWorkspaceDiscovery;
    let workspace = discovery
        .discover(root)
        .expect("Should discover workspace with mixed exclusions");

    assert_eq!(workspace.member_count(), 2);
    assert!(workspace.find_member("pkg-a").is_some());
    assert!(workspace.find_member("app-1").is_some());
    assert!(workspace.find_member("exclude-me").is_none());
    assert!(workspace.find_member("test-app").is_none());
}

#[cfg(feature = "discovery-rust")]
#[test]
fn test_integration_real_cargo_toml() {
    // This test attempts to discover the workspace of the current repo
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let discovery = CargoTomlDiscovery;

    // Check if we are actually in the repo (might differ in some CI envs or packaged builds)
    if !root.join("Cargo.toml").exists() {
        println!(
            "Skipping integration test: Cargo.toml not found at {}",
            root.display()
        );
        return;
    }

    let workspace = discovery
        .discover(root)
        .expect("Should discover real repo workspace");

    // We expect at least cuenv-workspaces to be a member
    assert!(workspace.member_count() > 0);
    assert!(workspace.find_member("cuenv-workspaces").is_some());

    // Check for other expected members
    assert!(workspace.find_member("cuenv-core").is_some());
    assert!(workspace.find_member("cuenv-cli").is_some());
    assert!(workspace.find_member("cuengine").is_some());
}
