//! Integration tests for the Node.js contrib tool.
//!
//! These tests keep network out of the execution path by:
//! - verifying `cuenv sync lock` expands official Node.js archive URLs
//! - seeding a fake extracted prefix into the tool cache
//! - asserting `cuenv exec` activates that prefix correctly

#![allow(clippy::unwrap_used, clippy::expect_used)]

use cuenv_core::lockfile::Lockfile;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

const CUENV_BIN: &str = env!("CARGO_BIN_EXE_cuenv");

fn clean_environment_command(bin: impl AsRef<OsStr>) -> Command {
    let mut cmd = Command::new(bin);
    cmd.env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default());
    cmd
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root should resolve")
}

fn copy_cue_dir_recursive(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if path.is_dir() {
            copy_cue_dir_recursive(&path, &dst_path);
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("cue") {
            fs::copy(path, dst_path).unwrap();
        }
    }
}

fn init_git_repo(root: &Path) {
    let init = Command::new("git")
        .args(["init"])
        .current_dir(root)
        .output()
        .expect("git init should run");
    assert!(
        init.status.success(),
        "git init failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );

    Command::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(root)
        .output()
        .expect("git email config should run");
    Command::new("git")
        .args(["config", "user.name", "Test User"])
        .current_dir(root)
        .output()
        .expect("git name config should run");
}

fn create_node_tool_repo(version: &str) -> (TempDir, PathBuf, PathBuf) {
    let temp_dir = tempfile::Builder::new()
        .prefix("cuenv_node_tool_")
        .tempdir()
        .expect("tempdir should create");
    let root = temp_dir.path().to_path_buf();
    let home = root.join("home");
    fs::create_dir_all(&home).unwrap();

    fs::create_dir_all(root.join("cue.mod")).unwrap();
    fs::write(
        root.join("cue.mod/module.cue"),
        "module: \"github.com/cuenv/cuenv\"\nlanguage: {\n\tversion: \"v0.9.0\"\n}\n",
    )
    .unwrap();

    copy_cue_dir_recursive(&repo_root().join("schema"), &root.join("schema"));
    copy_cue_dir_recursive(
        &repo_root().join("contrib/node"),
        &root.join("contrib/node"),
    );

    fs::write(
        root.join("env.cue"),
        format!(
            r#"package cuenv

import (
	"github.com/cuenv/cuenv/schema"
	xNode "github.com/cuenv/cuenv/contrib/node"
)

schema.#Project & {{
	name: "node-tool-test"

	runtime: schema.#ToolsRuntime & {{
		platforms: ["darwin-arm64", "darwin-x86_64", "linux-arm64", "linux-x86_64"]
		tools: {{
			node: xNode.#Node & {{version: "{version}"}}
		}}
	}}
}}
"#
        ),
    )
    .unwrap();

    init_git_repo(&root);
    (temp_dir, root, home)
}

fn run_cuenv(root: &Path, home: &Path, args: &[&str]) -> (String, String, bool) {
    let xdg_cache_home = home.join(".cache");
    let output = clean_environment_command(CUENV_BIN)
        .args(args)
        .current_dir(root)
        .env("HOME", home)
        .env("XDG_CACHE_HOME", &xdg_cache_home)
        .env("USER", "test-user")
        .env("CUENV_EXECUTABLE", CUENV_BIN)
        .output()
        .expect("cuenv command should run");

    (
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
        output.status.success(),
    )
}

fn cache_roots(home: &Path) -> [PathBuf; 2] {
    [home.join(".cache"), home.join("Library/Caches")]
}

fn write_executable(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(path, perms).unwrap();
    }
}

fn seed_node_prefix(home: &Path, version: &str, with_corepack: bool) {
    for cache_root in cache_roots(home) {
        let prefix = cache_root.join("cuenv/tools/url/node").join(version);
        write_executable(
            &prefix.join("bin/node"),
            &format!("#!/bin/sh\necho v{version}-cuenv-fixture\n"),
        );
        write_executable(
            &prefix.join("bin/npm"),
            &format!("#!/bin/sh\necho npm-{version}-cuenv-fixture\n"),
        );
        write_executable(
            &prefix.join("bin/npx"),
            &format!("#!/bin/sh\necho npx-{version}-cuenv-fixture\n"),
        );
        if with_corepack {
            write_executable(
                &prefix.join("bin/corepack"),
                &format!("#!/bin/sh\necho corepack-{version}-cuenv-fixture\n"),
            );
        }

        fs::create_dir_all(prefix.join("lib/node_modules/npm")).unwrap();
        fs::write(
            prefix.join("lib/node_modules/npm/package.json"),
            br#"{"name":"npm"}"#,
        )
        .unwrap();
        fs::create_dir_all(prefix.join("include/node")).unwrap();
        fs::write(
            prefix.join("include/node/node.h"),
            b"#define NODE_FIXTURE 1\n",
        )
        .unwrap();
    }
}

fn load_lockfile(root: &Path) -> Lockfile {
    Lockfile::load(&root.join("cuenv.lock"))
        .unwrap()
        .expect("lockfile should exist")
}

#[test]
fn sync_lock_resolves_node_24_lts_sources() {
    let (_tmp, root, home) = create_node_tool_repo("24.14.0");

    let (_stdout, stderr, success) = run_cuenv(&root, &home, &["sync", "lock"]);
    assert!(success, "sync lock failed: {stderr}");

    let lockfile = load_lockfile(&root);
    let node = lockfile.tools.get("node").expect("node tool should lock");
    assert_eq!(node.version, "24.14.0");
    assert_eq!(
        node.platforms["darwin-arm64"].source["url"].as_str(),
        Some("https://nodejs.org/dist/v24.14.0/node-v24.14.0-darwin-arm64.tar.gz")
    );
    assert_eq!(
        node.platforms["darwin-x86_64"].source["url"].as_str(),
        Some("https://nodejs.org/dist/v24.14.0/node-v24.14.0-darwin-x64.tar.gz")
    );
    assert_eq!(
        node.platforms["linux-arm64"].source["url"].as_str(),
        Some("https://nodejs.org/dist/v24.14.0/node-v24.14.0-linux-arm64.tar.gz")
    );
    assert_eq!(
        node.platforms["linux-x86_64"].source["url"].as_str(),
        Some("https://nodejs.org/dist/v24.14.0/node-v24.14.0-linux-x64.tar.gz")
    );
}

#[test]
fn sync_lock_resolves_node_25_sources() {
    let (_tmp, root, home) = create_node_tool_repo("25.8.1");

    let (_stdout, stderr, success) = run_cuenv(&root, &home, &["sync", "lock"]);
    assert!(success, "sync lock failed: {stderr}");

    let lockfile = load_lockfile(&root);
    let node = lockfile.tools.get("node").expect("node tool should lock");
    assert_eq!(node.version, "25.8.1");
    assert_eq!(
        node.platforms["darwin-arm64"].source["url"].as_str(),
        Some("https://nodejs.org/dist/v25.8.1/node-v25.8.1-darwin-arm64.tar.gz")
    );
    assert_eq!(
        node.platforms["darwin-x86_64"].source["url"].as_str(),
        Some("https://nodejs.org/dist/v25.8.1/node-v25.8.1-darwin-x64.tar.gz")
    );
    assert_eq!(
        node.platforms["linux-arm64"].source["url"].as_str(),
        Some("https://nodejs.org/dist/v25.8.1/node-v25.8.1-linux-arm64.tar.gz")
    );
    assert_eq!(
        node.platforms["linux-x86_64"].source["url"].as_str(),
        Some("https://nodejs.org/dist/v25.8.1/node-v25.8.1-linux-x64.tar.gz")
    );
}

#[test]
fn exec_activates_node_24_lts_prefix_with_corepack() {
    let (_tmp, root, home) = create_node_tool_repo("24.14.0");
    let (_stdout, stderr, success) = run_cuenv(&root, &home, &["sync", "lock"]);
    assert!(success, "sync lock failed: {stderr}");

    seed_node_prefix(&home, "24.14.0", true);

    let (stdout, stderr, success) = run_cuenv(&root, &home, &["exec", "--", "node", "--version"]);
    assert!(success, "node exec failed: {stderr}");
    assert_eq!(stdout.trim(), "v24.14.0-cuenv-fixture");

    let (stdout, stderr, success) = run_cuenv(&root, &home, &["exec", "--", "npm", "--version"]);
    assert!(success, "npm exec failed: {stderr}");
    assert_eq!(stdout.trim(), "npm-24.14.0-cuenv-fixture");

    let (_stdout, stderr, success) = run_cuenv(
        &root,
        &home,
        &[
            "exec",
            "--",
            "sh",
            "-c",
            "node_path=\"$(command -v node)\" && test -x \"${node_path%/*}/corepack\"",
        ],
    );
    assert!(success, "corepack should exist for Node 24: {stderr}");
}

#[test]
fn exec_activates_node_25_prefix_without_corepack() {
    let (_tmp, root, home) = create_node_tool_repo("25.8.1");
    let (_stdout, stderr, success) = run_cuenv(&root, &home, &["sync", "lock"]);
    assert!(success, "sync lock failed: {stderr}");

    seed_node_prefix(&home, "25.8.1", false);

    let (stdout, stderr, success) = run_cuenv(&root, &home, &["exec", "--", "node", "--version"]);
    assert!(success, "node exec failed: {stderr}");
    assert_eq!(stdout.trim(), "v25.8.1-cuenv-fixture");

    let (stdout, stderr, success) = run_cuenv(&root, &home, &["exec", "--", "npx", "--version"]);
    assert!(success, "npx exec failed: {stderr}");
    assert_eq!(stdout.trim(), "npx-25.8.1-cuenv-fixture");

    let (_stdout, stderr, success) = run_cuenv(
        &root,
        &home,
        &[
            "exec",
            "--",
            "sh",
            "-c",
            "node_path=\"$(command -v node)\" && test ! -e \"${node_path%/*}/corepack\"",
        ],
    );
    assert!(success, "corepack should be absent for Node 25: {stderr}");
}
