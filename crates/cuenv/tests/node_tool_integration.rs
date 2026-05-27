//! Integration tests for the Node.js contrib tool.
//!
//! These tests keep network out of the execution path by:
//! - verifying `cuenv sync lock` expands official Node.js archive URLs
//! - seeding a fake extracted prefix into the tool cache
//! - asserting `cuenv exec` activates that prefix correctly

use cuenv_core::lockfile::{LockedTool, Lockfile};
use std::error::Error;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

const CUENV_BIN: &str = env!("CARGO_BIN_EXE_cuenv");

fn clean_environment_command(bin: impl AsRef<OsStr>) -> Command {
    let mut cmd = Command::new(bin);
    cmd.env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default());
    cmd
}

struct CuenvOutput {
    stdout: String,
    stderr: String,
    success: bool,
}

fn repo_root() -> TestResult<PathBuf> {
    Ok(PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?)
}

fn copy_cue_dir_recursive(src: &Path, dst: &Path) -> TestResult {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if path.is_dir() {
            copy_cue_dir_recursive(&path, &dst_path)?;
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("cue") {
            fs::copy(path, dst_path)?;
        }
    }

    Ok(())
}

fn run_git(root: &Path, args: &[&str]) -> TestResult {
    let output = Command::new("git").args(args).current_dir(root).output()?;
    assert!(
        output.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );

    Ok(())
}

fn init_git_repo(root: &Path) -> TestResult {
    run_git(root, &["init"])?;
    run_git(root, &["config", "user.email", "test@example.com"])?;
    run_git(root, &["config", "user.name", "Test User"])?;

    Ok(())
}

fn create_node_tool_repo(version: &str) -> TestResult<(TempDir, PathBuf, PathBuf)> {
    let temp_dir = tempfile::Builder::new()
        .prefix("cuenv_node_tool_")
        .tempdir()?;
    let root = temp_dir.path().to_path_buf();
    let home = root.join("home");
    fs::create_dir_all(&home)?;

    fs::create_dir_all(root.join("cue.mod"))?;
    fs::write(
        root.join("cue.mod/module.cue"),
        "module: \"github.com/cuenv/cuenv\"\nlanguage: {\n\tversion: \"v0.9.0\"\n}\n",
    )?;

    let repo_root = repo_root()?;
    copy_cue_dir_recursive(&repo_root.join("schema"), &root.join("schema"))?;
    copy_cue_dir_recursive(&repo_root.join("contrib/node"), &root.join("contrib/node"))?;

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
    )?;

    init_git_repo(&root)?;
    Ok((temp_dir, root, home))
}

fn run_cuenv(root: &Path, home: &Path, args: &[&str]) -> TestResult<CuenvOutput> {
    let xdg_cache_home = home.join(".cache");
    let output = clean_environment_command(CUENV_BIN)
        .args(args)
        .current_dir(root)
        .env("HOME", home)
        .env("XDG_CACHE_HOME", &xdg_cache_home)
        .env("USER", "test-user")
        .env("CUENV_EXECUTABLE", CUENV_BIN)
        .output()?;

    Ok(CuenvOutput {
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        success: output.status.success(),
    })
}

fn cache_roots(home: &Path) -> [PathBuf; 2] {
    [home.join(".cache"), home.join("Library/Caches")]
}

fn write_executable(path: &Path, contents: &str) -> TestResult {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, contents)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(path)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(path, perms)?;
    }

    Ok(())
}

fn seed_node_prefix(home: &Path, version: &str, with_corepack: bool) -> TestResult {
    for cache_root in cache_roots(home) {
        let prefix = cache_root.join("cuenv/tools/url/node").join(version);
        write_executable(
            &prefix.join("bin/node"),
            &format!("#!/bin/sh\necho v{version}-cuenv-fixture\n"),
        )?;
        write_executable(
            &prefix.join("bin/npm"),
            &format!("#!/bin/sh\necho npm-{version}-cuenv-fixture\n"),
        )?;
        write_executable(
            &prefix.join("bin/npx"),
            &format!("#!/bin/sh\necho npx-{version}-cuenv-fixture\n"),
        )?;
        if with_corepack {
            write_executable(
                &prefix.join("bin/corepack"),
                &format!("#!/bin/sh\necho corepack-{version}-cuenv-fixture\n"),
            )?;
        }

        fs::create_dir_all(prefix.join("lib/node_modules/npm"))?;
        fs::write(
            prefix.join("lib/node_modules/npm/package.json"),
            br#"{"name":"npm"}"#,
        )?;
        fs::create_dir_all(prefix.join("include/node"))?;
        fs::write(
            prefix.join("include/node/node.h"),
            b"#define NODE_FIXTURE 1\n",
        )?;
    }

    Ok(())
}

fn load_lockfile(root: &Path) -> TestResult<Lockfile> {
    Ok(Lockfile::load(&root.join("cuenv.lock"))?
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "lockfile should exist"))?)
}

fn locked_node(lockfile: &Lockfile) -> TestResult<&LockedTool> {
    Ok(lockfile
        .tools
        .get("node")
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "node tool should lock"))?)
}

fn node_source_url<'a>(node: &'a LockedTool, platform: &str) -> TestResult<&'a str> {
    let locked_platform = node.platforms.get(platform).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("node tool should lock {platform}"),
        )
    })?;

    Ok(locked_platform
        .source
        .get("url")
        .and_then(|url| url.as_str())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("node {platform} source should include a URL"),
            )
        })?)
}

#[test]
fn sync_lock_resolves_node_24_lts_sources() -> TestResult {
    let (_tmp, root, home) = create_node_tool_repo("24.14.0")?;

    let output = run_cuenv(&root, &home, &["sync", "lock"])?;
    assert!(output.success, "sync lock failed: {}", output.stderr);

    let lockfile = load_lockfile(&root)?;
    let node = locked_node(&lockfile)?;
    assert_eq!(node.version, "24.14.0");
    assert_eq!(
        node_source_url(node, "darwin-arm64")?,
        "https://nodejs.org/dist/v24.14.0/node-v24.14.0-darwin-arm64.tar.gz"
    );
    assert_eq!(
        node_source_url(node, "darwin-x86_64")?,
        "https://nodejs.org/dist/v24.14.0/node-v24.14.0-darwin-x64.tar.gz"
    );
    assert_eq!(
        node_source_url(node, "linux-arm64")?,
        "https://nodejs.org/dist/v24.14.0/node-v24.14.0-linux-arm64.tar.gz"
    );
    assert_eq!(
        node_source_url(node, "linux-x86_64")?,
        "https://nodejs.org/dist/v24.14.0/node-v24.14.0-linux-x64.tar.gz"
    );

    Ok(())
}

#[test]
fn sync_lock_resolves_node_25_sources() -> TestResult {
    let (_tmp, root, home) = create_node_tool_repo("25.8.1")?;

    let output = run_cuenv(&root, &home, &["sync", "lock"])?;
    assert!(output.success, "sync lock failed: {}", output.stderr);

    let lockfile = load_lockfile(&root)?;
    let node = locked_node(&lockfile)?;
    assert_eq!(node.version, "25.8.1");
    assert_eq!(
        node_source_url(node, "darwin-arm64")?,
        "https://nodejs.org/dist/v25.8.1/node-v25.8.1-darwin-arm64.tar.gz"
    );
    assert_eq!(
        node_source_url(node, "darwin-x86_64")?,
        "https://nodejs.org/dist/v25.8.1/node-v25.8.1-darwin-x64.tar.gz"
    );
    assert_eq!(
        node_source_url(node, "linux-arm64")?,
        "https://nodejs.org/dist/v25.8.1/node-v25.8.1-linux-arm64.tar.gz"
    );
    assert_eq!(
        node_source_url(node, "linux-x86_64")?,
        "https://nodejs.org/dist/v25.8.1/node-v25.8.1-linux-x64.tar.gz"
    );

    Ok(())
}

#[test]
fn exec_activates_node_24_lts_prefix_with_corepack() -> TestResult {
    let (_tmp, root, home) = create_node_tool_repo("24.14.0")?;
    let output = run_cuenv(&root, &home, &["sync", "lock"])?;
    assert!(output.success, "sync lock failed: {}", output.stderr);

    seed_node_prefix(&home, "24.14.0", true)?;

    let output = run_cuenv(&root, &home, &["exec", "--", "node", "--version"])?;
    assert!(output.success, "node exec failed: {}", output.stderr);
    assert_eq!(output.stdout.trim(), "v24.14.0-cuenv-fixture");

    let output = run_cuenv(&root, &home, &["exec", "--", "npm", "--version"])?;
    assert!(output.success, "npm exec failed: {}", output.stderr);
    assert_eq!(output.stdout.trim(), "npm-24.14.0-cuenv-fixture");

    let output = run_cuenv(
        &root,
        &home,
        &[
            "exec",
            "--",
            "sh",
            "-c",
            "node_path=\"$(command -v node)\" && test -x \"${node_path%/*}/corepack\"",
        ],
    )?;
    assert!(
        output.success,
        "corepack should exist for Node 24: {}",
        output.stderr
    );

    Ok(())
}

#[test]
fn exec_activates_node_25_prefix_without_corepack() -> TestResult {
    let (_tmp, root, home) = create_node_tool_repo("25.8.1")?;
    let output = run_cuenv(&root, &home, &["sync", "lock"])?;
    assert!(output.success, "sync lock failed: {}", output.stderr);

    seed_node_prefix(&home, "25.8.1", false)?;

    let output = run_cuenv(&root, &home, &["exec", "--", "node", "--version"])?;
    assert!(output.success, "node exec failed: {}", output.stderr);
    assert_eq!(output.stdout.trim(), "v25.8.1-cuenv-fixture");

    let output = run_cuenv(&root, &home, &["exec", "--", "npx", "--version"])?;
    assert!(output.success, "npx exec failed: {}", output.stderr);
    assert_eq!(output.stdout.trim(), "npx-25.8.1-cuenv-fixture");

    let output = run_cuenv(
        &root,
        &home,
        &[
            "exec",
            "--",
            "sh",
            "-c",
            "node_path=\"$(command -v node)\" && test ! -e \"${node_path%/*}/corepack\"",
        ],
    )?;
    assert!(
        output.success,
        "corepack should be absent for Node 25: {}",
        output.stderr
    );

    Ok(())
}
