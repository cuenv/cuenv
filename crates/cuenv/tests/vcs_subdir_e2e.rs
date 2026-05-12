//! End-to-end coverage for VCS subdir sync examples.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

const CUENV_BIN: &str = env!("CARGO_BIN_EXE_cuenv");
const PUBLIC_CUENV_URL: &str = "https://github.com/cuenv/cuenv.git";

fn clean_environment_command(bin: impl AsRef<OsStr>) -> Command {
    let mut cmd = Command::new(bin);
    cmd.env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", std::env::var("HOME").unwrap_or_default())
        .env("USER", std::env::var("USER").unwrap_or_default());
    cmd
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root should resolve")
}

fn copy_dir_recursive(src: &Path, dst: &Path, only_cue_files: bool) {
    fs::create_dir_all(dst).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        let file_name = path.file_name().unwrap();
        let dst_path = dst.join(file_name);

        if path.is_dir() {
            copy_dir_recursive(&path, &dst_path, only_cue_files);
        } else if !only_cue_files || path.extension().and_then(|s| s.to_str()) == Some("cue") {
            fs::copy(&path, &dst_path).unwrap();
        }
    }
}

fn run_git(args: &[&str], cwd: &Path) {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("git command should run");
    assert!(
        output.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn init_git_repo(root: &Path) {
    run_git(&["init", "-b", "main"], root);
    run_git(&["config", "user.email", "test@example.com"], root);
    run_git(&["config", "user.name", "Cuenv Test"], root);
    run_git(&["config", "commit.gpgsign", "false"], root);
}

fn create_agent_skills_source_repo() -> TempDir {
    let source = tempfile::Builder::new()
        .prefix("cuenv_source_")
        .tempdir()
        .expect("source tempdir");
    init_git_repo(source.path());

    copy_dir_recursive(
        &repo_root().join(".agents/skills"),
        &source.path().join(".agents/skills"),
        false,
    );
    run_git(&["add", ".agents/skills"], source.path());
    run_git(&["commit", "-m", "seed agent skills"], source.path());
    source
}

fn write_local_cuenv_module(root: &Path) {
    fs::create_dir_all(root.join("cue.mod")).unwrap();
    fs::write(
        root.join("cue.mod/module.cue"),
        "module: \"github.com/cuenv/cuenv\"\nlanguage: {\n\tversion: \"v0.9.0\"\n}\n",
    )
    .unwrap();

    copy_dir_recursive(&repo_root().join("schema"), &root.join("schema"), true);
}

fn cue_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn create_example_target_repo(source: &Path) -> TempDir {
    let target = tempfile::Builder::new()
        .prefix("cuenv_target_")
        .tempdir()
        .expect("target tempdir");
    init_git_repo(target.path());
    write_local_cuenv_module(target.path());

    let example = fs::read_to_string(repo_root().join("examples/vcs-subdir/env.cue"))
        .expect("read vcs-subdir example")
        .replace(PUBLIC_CUENV_URL, &cue_string(&source.display().to_string()));
    fs::write(target.path().join("env.cue"), example).unwrap();
    target
}

fn run_cuenv(args: &[&str], cwd: &Path) -> std::process::Output {
    clean_environment_command(CUENV_BIN)
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("cuenv command should run")
}

#[test]
fn vcs_subdir_example_syncs_agent_skills_without_vendor_requirement() {
    let source = create_agent_skills_source_repo();
    let target = create_example_target_repo(source.path());

    let target_arg = target.path().to_str().unwrap();
    let sync = run_cuenv(
        &["sync", "vcs", "--path", target_arg, "--package", "examples"],
        target.path(),
    );
    assert!(
        sync.status.success(),
        "cuenv sync vcs failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&sync.stdout),
        String::from_utf8_lossy(&sync.stderr)
    );

    assert!(
        target
            .path()
            .join(".agents/skills/cuenv-schema-first/SKILL.md")
            .exists()
    );
    assert!(
        target
            .path()
            .join(".agents/skills/cuenv-tools-lock-vcs/SKILL.md")
            .exists()
    );
    assert!(!target.path().join(".agents/skills/.git").exists());
    assert!(target.path().join(".agents/skills/.cuenv-vcs").exists());

    let gitignore = fs::read_to_string(target.path().join(".gitignore")).unwrap();
    assert!(gitignore.contains(".agents/skills/"));

    let lockfile = fs::read_to_string(target.path().join("cuenv.lock")).unwrap();
    assert!(lockfile.contains("vendor = false"));
    assert!(lockfile.contains("path = \".agents/skills\""));
    assert!(lockfile.contains("subdir = \".agents/skills\""));
    assert!(lockfile.contains("subtree = "));

    let check = run_cuenv(
        &[
            "sync",
            "vcs",
            "--path",
            target_arg,
            "--package",
            "examples",
            "--check",
        ],
        target.path(),
    );
    assert!(
        check.status.success(),
        "cuenv sync vcs --check failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&check.stdout),
        String::from_utf8_lossy(&check.stderr)
    );

    let inspect = run_cuenv(
        &[
            "task",
            "--path",
            target_arg,
            "--package",
            "examples",
            "inspect",
        ],
        target.path(),
    );
    assert!(
        inspect.status.success(),
        "cuenv task inspect failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&inspect.stdout),
        String::from_utf8_lossy(&inspect.stderr)
    );
    let stdout = String::from_utf8_lossy(&inspect.stdout);
    assert!(stdout.contains(".agents/skills/cuenv-schema-first/SKILL.md"));
}
