#![allow(missing_docs)]

use cuengine::evaluate_cue_package_typed;
use cuenv_core::manifest::{Base, Project};
use std::error::Error;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

/// Create a Command with a clean environment (no CI vars leaking).
fn clean_environment_command(bin: impl AsRef<OsStr>) -> Command {
    let mut cmd = Command::new(bin);
    cmd.env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", std::env::var("HOME").unwrap_or_default())
        .env("USER", std::env::var("USER").unwrap_or_default());
    cmd
}

fn create_test_dir() -> TestResult<TempDir> {
    Ok(tempfile::Builder::new().prefix("cuenv_test_").tempdir()?)
}

fn repo_root() -> TestResult<PathBuf> {
    Ok(PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?)
}

fn write_local_cuenv_module(root: &Path) -> TestResult {
    fs::create_dir_all(root.join("cue.mod"))?;
    fs::write(
        root.join("cue.mod/module.cue"),
        "module: \"github.com/cuenv/cuenv\"\nlanguage: {\n\tversion: \"v0.9.0\"\n}\n",
    )?;

    // Copy the real schema package into the temporary module so imports work.
    let schema_src = repo_root()?.join("schema");
    let schema_dst = root.join("schema");
    copy_dir_recursive(&schema_src, &schema_dst)
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> TestResult {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let path = entry.path();
        let dst_path = dst.join(entry.file_name());

        if path.is_dir() {
            copy_dir_recursive(&path, &dst_path)?;
        } else if path.extension().and_then(|s| s.to_str()) == Some("cue") {
            fs::copy(&path, &dst_path)?;
        }
    }
    Ok(())
}

#[test]
fn project_name_is_required_by_schema() -> TestResult {
    let tmp = create_test_dir()?;
    let root = tmp.path();
    write_local_cuenv_module(root)?;

    fs::write(
        root.join("env.cue"),
        r#"package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
  // name intentionally omitted
}
"#,
    )?;

    let res = evaluate_cue_package_typed::<Project>(root, "cuenv");
    assert!(res.is_err(), "schema should reject missing `name`");
    Ok(())
}

#[test]
fn project_name_cannot_be_empty() -> TestResult {
    let tmp = create_test_dir()?;
    let root = tmp.path();
    write_local_cuenv_module(root)?;

    fs::write(
        root.join("env.cue"),
        r#"package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
  name: ""
}
"#,
    )?;

    let res = evaluate_cue_package_typed::<Project>(root, "cuenv");
    assert!(res.is_err(), "schema should reject empty `name`");
    Ok(())
}

#[test]
fn vcs_dependency_name_accepts_safe_names() -> TestResult {
    let tmp = create_test_dir()?;
    let root = tmp.path();
    write_local_cuenv_module(root)?;

    fs::write(
        root.join("env.cue"),
        r#"package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
  name: "app"
  vcs: {
    "lib.core-1": {
      url: "https://github.com/example/lib.git"
      vendor: true
      path: "vendor/lib"
    }
  }
}
"#,
    )?;

    let project = evaluate_cue_package_typed::<Project>(root, "cuenv")?;
    assert!(project.vcs.contains_key("lib.core-1"));
    Ok(())
}

#[test]
fn vcs_dependency_name_rejects_runtime_invalid_names() -> TestResult {
    for name in [".lib", "lib..core"] {
        let tmp = create_test_dir()?;
        let root = tmp.path();
        write_local_cuenv_module(root)?;

        fs::write(
            root.join("env.cue"),
            format!(
                r#"package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {{
  name: "app"
  vcs: {{
    "{name}": {{
      url: "https://github.com/example/lib.git"
      vendor: true
      path: "vendor/lib"
    }}
  }}
}}
"#
            ),
        )?;

        let res = evaluate_cue_package_typed::<Project>(root, "cuenv");
        assert!(
            res.is_err(),
            "schema should reject VCS dependency name {name}"
        );
    }
    Ok(())
}

#[test]
fn base_can_be_composed_standalone() -> TestResult {
    let tmp = create_test_dir()?;
    let root = tmp.path();
    write_local_cuenv_module(root)?;

    fs::write(
        root.join("env.cue"),
        r#"package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Base & {
  env: {
    HELLO: "world"
  }
}
"#,
    )?;

    let base = evaluate_cue_package_typed::<Base>(root, "cuenv")?;
    assert!(base.env.is_some());
    Ok(())
}

#[test]
fn task_command_with_base_schema_shows_helpful_error() -> TestResult {
    let tmp = create_test_dir()?;
    let root = tmp.path();
    write_local_cuenv_module(root)?;

    fs::write(
        root.join("env.cue"),
        r#"package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Base & {
  env: {
    HELLO: "world"
  }
}
"#,
    )?;

    // Try to execute task command (which requires schema.#Project)
    let cuenv_bin = env!("CARGO_BIN_EXE_cuenv");
    let root_arg = root.to_str().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "test path is not UTF-8")
    })?;
    let output = clean_environment_command(cuenv_bin)
        .args(["task", "--path", root_arg])
        .output()?;

    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!output.status.success(), "should fail with Base schema");
    // Note: miette wraps long lines with │ characters, so we check for parts separately
    // to avoid failures due to line-break positions.
    assert!(
        stderr.contains("schema.#Base")
            && stderr.contains("doesn't")
            && stderr.contains("support tasks"),
        "error message should explain Base schema doesn't support tasks, got: {stderr}",
    );
    Ok(())
}
