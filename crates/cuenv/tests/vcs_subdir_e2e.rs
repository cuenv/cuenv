//! End-to-end coverage for VCS subdir sync examples.

use std::error::Error;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

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

fn repo_root() -> TestResult<PathBuf> {
    Ok(PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?)
}

fn copy_dir_recursive(src: &Path, dst: &Path, only_cue_files: bool) -> TestResult {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let path = entry.path();
        let dst_path = dst.join(entry.file_name());

        if path.is_dir() {
            copy_dir_recursive(&path, &dst_path, only_cue_files)?;
        } else if !only_cue_files || path.extension().and_then(|s| s.to_str()) == Some("cue") {
            fs::copy(&path, &dst_path)?;
        }
    }
    Ok(())
}

fn run_git(args: &[&str], cwd: &Path) -> TestResult {
    let output = Command::new("git").args(args).current_dir(cwd).output()?;
    assert!(
        output.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

fn init_git_repo(root: &Path) -> TestResult {
    run_git(&["init", "-b", "main"], root)?;
    run_git(&["config", "user.email", "test@example.com"], root)?;
    run_git(&["config", "user.name", "Cuenv Test"], root)?;
    run_git(&["config", "commit.gpgsign", "false"], root)
}

fn create_agent_skills_source_repo() -> TestResult<TempDir> {
    let source = tempfile::Builder::new().prefix("cuenv_source_").tempdir()?;
    init_git_repo(source.path())?;

    copy_dir_recursive(
        &repo_root()?.join(".agents/skills"),
        &source.path().join(".agents/skills"),
        false,
    )?;
    run_git(&["add", ".agents/skills"], source.path())?;
    run_git(&["commit", "-m", "seed agent skills"], source.path())?;
    Ok(source)
}

fn write_local_cuenv_module(root: &Path) -> TestResult {
    fs::create_dir_all(root.join("cue.mod"))?;
    fs::write(
        root.join("cue.mod/module.cue"),
        "module: \"github.com/cuenv/cuenv\"\nlanguage: {\n\tversion: \"v0.9.0\"\n}\n",
    )?;

    copy_dir_recursive(&repo_root()?.join("schema"), &root.join("schema"), true)
}

fn cue_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn create_example_target_repo(source: &Path) -> TestResult<TempDir> {
    let target = tempfile::Builder::new().prefix("cuenv_target_").tempdir()?;
    init_git_repo(target.path())?;
    write_local_cuenv_module(target.path())?;

    let example = fs::read_to_string(repo_root()?.join("examples/vcs-subdir/env.cue"))?
        .replace(PUBLIC_CUENV_URL, &cue_string(&source.display().to_string()));
    fs::write(target.path().join("env.cue"), example)?;
    Ok(target)
}

fn run_cuenv(args: &[&str], cwd: &Path) -> TestResult<std::process::Output> {
    Ok(clean_environment_command(CUENV_BIN)
        .args(args)
        .current_dir(cwd)
        .output()?)
}

#[test]
fn vcs_subdir_example_syncs_agent_skills_without_vendor_requirement() -> TestResult {
    let source = create_agent_skills_source_repo()?;
    let target = create_example_target_repo(source.path())?;

    let target_arg = target_path_arg(&target)?;
    let sync = run_cuenv(
        &[
            "sync",
            "vcs",
            "--path",
            &target_arg,
            "--package",
            "examples",
        ],
        target.path(),
    )?;
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

    let gitignore = fs::read_to_string(target.path().join(".gitignore"))?;
    assert!(gitignore.contains(".agents/skills/"));

    let lockfile = fs::read_to_string(target.path().join("cuenv.lock"))?;
    assert!(lockfile.contains("vendor = false"));
    assert!(lockfile.contains("path = \".agents/skills\""));
    assert!(lockfile.contains("subdir = \".agents/skills\""));
    assert!(lockfile.contains("subtree = "));

    let check = run_cuenv(
        &[
            "sync",
            "vcs",
            "--path",
            &target_arg,
            "--package",
            "examples",
            "--check",
        ],
        target.path(),
    )?;
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
            &target_arg,
            "--package",
            "examples",
            "inspect",
        ],
        target.path(),
    )?;
    assert!(
        inspect.status.success(),
        "cuenv task inspect failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&inspect.stdout),
        String::from_utf8_lossy(&inspect.stderr)
    );
    let stdout = String::from_utf8_lossy(&inspect.stdout);
    assert!(stdout.contains(".agents/skills/cuenv-schema-first/SKILL.md"));
    Ok(())
}

fn create_overlay_target_repo(source: &Path) -> TestResult<TempDir> {
    let target = tempfile::Builder::new()
        .prefix("cuenv_overlay_target_")
        .tempdir()?;
    init_git_repo(target.path())?;
    write_local_cuenv_module(target.path())?;

    let env_cue = format!(
        "package examples\n\nimport \"github.com/cuenv/cuenv/schema\"\n\nschema.#Project & {{\n\tname: \"vcs-overlay\"\n\n\tvcs: \"agent-skills\": {{\n\t\turl:       \"{url}\"\n\t\treference: \"main\"\n\t\tvendor:    false\n\t\tsubdir:    \".agents/skills\"\n\t\tpath:      \".agents/skills\"\n\t\toverlay:   true\n\t}}\n}}\n",
        url = cue_string(&source.display().to_string()),
    );
    fs::write(target.path().join("env.cue"), env_cue)?;
    Ok(target)
}

#[test]
fn vcs_overlay_example_syncs_children_individually() -> TestResult {
    let source = create_agent_skills_source_repo()?;
    let target = create_overlay_target_repo(source.path())?;

    // A hand-written skill the user keeps in the same directory as the synced ones.
    let local = target.path().join(".agents/skills/local-skill");
    fs::create_dir_all(&local)?;
    fs::write(local.join("SKILL.md"), "# Local skill\n")?;

    let target_arg = target_path_arg(&target)?;
    let sync = run_cuenv(
        &[
            "sync",
            "vcs",
            "--path",
            &target_arg,
            "--package",
            "examples",
        ],
        target.path(),
    )?;
    assert!(
        sync.status.success(),
        "cuenv sync vcs (overlay) failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&sync.stdout),
        String::from_utf8_lossy(&sync.stderr)
    );

    // Synced children land individually, each independently owned.
    assert!(
        target
            .path()
            .join(".agents/skills/cuenv-schema-first/SKILL.md")
            .exists()
    );
    assert!(
        target
            .path()
            .join(".agents/skills/cuenv-schema-first/.cuenv-vcs")
            .exists()
    );
    // The parent dir is not owned: no top-level marker.
    assert!(!target.path().join(".agents/skills/.cuenv-vcs").exists());
    // Repo-local sibling survives.
    assert!(local.join("SKILL.md").exists());

    let gitignore = fs::read_to_string(target.path().join(".gitignore"))?;
    assert!(gitignore.contains(".agents/skills/cuenv-schema-first/"));
    assert!(
        !gitignore
            .lines()
            .any(|line| line.trim() == ".agents/skills/"),
        "parent dir must not be blanket-ignored: {gitignore}"
    );
    assert!(
        !gitignore.contains("local-skill"),
        "repo-local skill must not be gitignored: {gitignore}"
    );

    let lockfile = fs::read_to_string(target.path().join("cuenv.lock"))?;
    assert!(lockfile.contains("overlay = true"));

    // Re-running --check must pass without re-materializing.
    let check = run_cuenv(
        &[
            "sync",
            "vcs",
            "--path",
            &target_arg,
            "--package",
            "examples",
            "--check",
        ],
        target.path(),
    )?;
    assert!(
        check.status.success(),
        "cuenv sync vcs --check (overlay) failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&check.stdout),
        String::from_utf8_lossy(&check.stderr)
    );
    Ok(())
}

fn target_path_arg(target: &TempDir) -> TestResult<String> {
    target.path().to_str().map(str::to_owned).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "target path is not UTF-8").into()
    })
}
