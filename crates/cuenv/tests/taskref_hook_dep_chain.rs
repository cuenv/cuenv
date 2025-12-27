// Integration tests can use unwrap/expect for cleaner assertions
#![allow(
    missing_docs,
    unused_variables,
    clippy::too_many_lines,
    clippy::uninlined_format_args,
    clippy::unwrap_used,
    clippy::expect_used
)]

use std::fs;
use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

/// Create a test directory with proper prefix (non-hidden) for CUE loader compatibility.
///
/// CUE's `load.Instances` ignores directories starting with `.` (hidden directories).
/// The default `TempDir::new()` creates hidden directories like `.tmpXXXXX`, which causes
/// CUE evaluation to fail with "No instances could be evaluated".
fn create_test_dir() -> TempDir {
    tempfile::Builder::new()
        .prefix("cuenv_test_")
        .tempdir()
        .expect("Failed to create temp directory")
}

fn run_cuenv(args: &[&str]) -> (String, String, bool) {
    let cuenv_bin = env!("CARGO_BIN_EXE_cuenv");
    let output = Command::new(cuenv_bin)
        .args(args)
        .output()
        .expect("Failed to run cuenv");

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let success = output.status.success();

    (stdout, stderr, success)
}

#[cfg(unix)]
fn run_cuenv_with_path(args: &[&str], path_prefix: &Path) -> (String, String, bool) {
    let cuenv_bin = env!("CARGO_BIN_EXE_cuenv");
    let mut cmd = Command::new(cuenv_bin);
    cmd.args(args);

    let existing_path = std::env::var("PATH").unwrap_or_default();
    let combined = format!("{}:{}", path_prefix.display(), existing_path);
    cmd.env("PATH", combined);

    let output = cmd.output().expect("Failed to run cuenv");

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let success = output.status.success();

    (stdout, stderr, success)
}

fn write_cue_module(root: &Path) {
    fs::create_dir_all(root.join("cue.mod")).unwrap();
    fs::write(
        root.join("cue.mod/module.cue"),
        "module: \"test.com\"\nlanguage: {\n\tversion: \"v0.9.0\"\n}\n",
    )
    .unwrap();
}

#[test]
#[allow(clippy::too_many_lines)]
fn test_before_install_taskref_executes_transitive_dep_chain() {
    let tmp = create_test_dir();
    let root = tmp.path();

    // Some execution paths look for a git root.
    fs::create_dir_all(root.join(".git")).unwrap();
    write_cue_module(root);

    // Project B: referenced via TaskRef (name = projen-generator)
    // All projects must use `package cuenv` - this is enforced by cuenv
    let proj_b = root.join("projen-generator");
    fs::create_dir_all(&proj_b).unwrap();
    fs::write(
        proj_b.join("env.cue"),
        r#"package cuenv

name: "projen-generator"

env: {}

tasks: {
  install: {
    command: "sh"
    args: ["-c", "echo B-install"]
  }
  types: {
    command: "sh"
    args: ["-c", "echo B-types"]
    dependsOn: ["install"]
  }
}
"#,
    )
    .unwrap();

    // Project A: bun workspace with beforeInstall hook referencing B.types
    let proj_a = root.join("website");
    fs::create_dir_all(&proj_a).unwrap();
    fs::write(
        proj_a.join("env.cue"),
        r##"package cuenv

name: "website"

env: {}

workspaces: {
  bun: {
    hooks: {
      beforeInstall: [
        { ref: "#projen-generator:types" },
        {
          name: "projen"
          match: {
            labels: ["projen"]
          }
        },
      ]
    }
  }
}

tasks: {
  bun: {
    install: {
      command: "sh"
      args: ["-c", "echo A-bun-install"]
    }
  }
  dev: {
    command: "sh"
    args: ["-c", "echo A-dev"]
    workspaces: ["bun"]
  }
}
"##,
    )
    .unwrap();

    // Another project with a task matched by labels: ["projen"].
    let gen_proj = root.join("gen-proj");
    fs::create_dir_all(&gen_proj).unwrap();
    fs::write(
        gen_proj.join("env.cue"),
        r#"package cuenv

name: "gen-proj"

env: {}

tasks: {
  generate: {
    command: "sh"
    args: ["-c", "echo GEN"]
    labels: ["projen"]
  }
}
"#,
    )
    .unwrap();

    let (stdout, stderr, success) = run_cuenv(&[
        "task",
        "-p",
        proj_a.to_str().unwrap(),
        "--package",
        "cuenv",
        "dev",
    ]);

    assert!(
        success,
        "Expected success.\n--- stdout ---\n{}\n--- stderr ---\n{}",
        stdout, stderr
    );

    // Ensure the transitive chain executed in order:
    // B-install -> B-types -> GEN -> A-bun-install -> A-dev
    let p_install = stdout
        .find("B-install")
        .unwrap_or_else(|| panic!("expected B-install output\n--- stdout ---\n{}", stdout));
    let p_types = stdout
        .find("B-types")
        .unwrap_or_else(|| panic!("expected B-types output\n--- stdout ---\n{}", stdout));
    let p_gen = stdout
        .find("GEN")
        .unwrap_or_else(|| panic!("expected GEN output\n--- stdout ---\n{}", stdout));
    let p_a_install = stdout
        .find("A-bun-install")
        .unwrap_or_else(|| panic!("expected A-bun-install output\n--- stdout ---\n{}", stdout));
    let p_dev = stdout
        .find("A-dev")
        .unwrap_or_else(|| panic!("expected A-dev output\n--- stdout ---\n{}", stdout));

    assert!(p_install < p_types, "install should run before types");
    assert!(
        p_types < p_gen,
        "types should run before matcher hook tasks"
    );
    assert!(
        p_gen < p_a_install,
        "matcher hook tasks should run before bun.install"
    );
    assert!(p_a_install < p_dev, "bun.install should run before dev");
}

#[test]
#[cfg(unix)]
#[allow(clippy::too_many_lines)]
fn test_match_hooks_run_before_injected_bun_install() {
    let tmp = create_test_dir();
    let root = tmp.path();

    fs::create_dir_all(root.join(".git")).unwrap();
    write_cue_module(root);

    // Provide a stub `bun` binary so the injected bun.install task can run without
    // requiring bun to be installed on the test machine.
    let bin_dir = root.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let bun_path = bin_dir.join("bun");
    fs::write(
        &bun_path,
        "#!/bin/sh\nif [ \"$1\" = \"install\" ]; then\n  echo A-bun-install\n  exit 0\nfi\necho unexpected bun args: \"$@\" >&2\nexit 1\n",
    )
    .unwrap();
    let mut perms = fs::metadata(&bun_path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&bun_path, perms).unwrap();

    // Project B: referenced via TaskRef (name = projen-generator)
    // All projects must use `package cuenv` - this is enforced by cuenv
    let proj_b = root.join("projen-generator");
    fs::create_dir_all(&proj_b).unwrap();
    fs::write(
        proj_b.join("env.cue"),
        r#"package cuenv

name: "projen-generator"

env: {}

tasks: {
  install: {
    command: "sh"
    args: ["-c", "echo B-install"]
  }
  types: {
    command: "sh"
    args: ["-c", "echo B-types"]
    dependsOn: ["install"]
  }
}
"#,
    )
    .unwrap();

    // Another project with a task matched by labels: ["projen"].
    let gen_proj = root.join("gen-proj");
    fs::create_dir_all(&gen_proj).unwrap();
    fs::write(
        gen_proj.join("env.cue"),
        r#"package cuenv

name: "gen-proj"

env: {}

tasks: {
  generate: {
    command: "sh"
    args: ["-c", "echo GEN"]
    labels: ["projen"]
  }
}
"#,
    )
    .unwrap();

    // Project A: bun workspace with beforeInstall hook referencing B.types.
    // The inject config provides bun.install, which is injected by with_implicit_tasks().
    let proj_a = root.join("website");
    fs::create_dir_all(&proj_a).unwrap();
    fs::write(proj_a.join("package.json"), "{}\n").unwrap();
    fs::write(proj_a.join("bun.lock"), "\n").unwrap();
    fs::write(
        proj_a.join("env.cue"),
        r##"package cuenv

name: "website"

env: {}

workspaces: {
  bun: {
    commands: ["bun", "bunx"]
    inject: {
      install: {
        command: "bun"
        args: ["install"]
        hermetic: false
      }
    }
    hooks: {
      beforeInstall: [
        { ref: "#projen-generator:types" },
        {
          name: "projen"
          match: {
            labels: ["projen"]
          }
        },
      ]
    }
  }
}

tasks: {
  dev: {
    command: "sh"
    args: ["-c", "echo A-dev"]
    workspaces: ["bun"]
  }
}
"##,
    )
    .unwrap();

    let (stdout, stderr, success) = run_cuenv_with_path(
        &[
            "task",
            "-p",
            proj_a.to_str().unwrap(),
            "--package",
            "cuenv",
            "dev",
        ],
        &bin_dir,
    );

    assert!(
        success,
        "Expected success.\n--- stdout ---\n{}\n--- stderr ---\n{}",
        stdout, stderr
    );

    // Ensure ordering includes the matched tasks before bun.install.
    // B-install -> B-types -> GEN -> A-bun-install -> A-dev
    let p_install = stdout
        .find("B-install")
        .unwrap_or_else(|| panic!("expected B-install output\n--- stdout ---\n{}", stdout));
    let p_types = stdout
        .find("B-types")
        .unwrap_or_else(|| panic!("expected B-types output\n--- stdout ---\n{}", stdout));
    let p_gen = stdout
        .find("GEN")
        .unwrap_or_else(|| panic!("expected GEN output\n--- stdout ---\n{}", stdout));
    let p_a_install = stdout
        .find("A-bun-install")
        .unwrap_or_else(|| panic!("expected A-bun-install output\n--- stdout ---\n{}", stdout));
    let p_dev = stdout
        .find("A-dev")
        .unwrap_or_else(|| panic!("expected A-dev output\n--- stdout ---\n{}", stdout));

    assert!(p_install < p_types, "install should run before types");
    assert!(
        p_types < p_gen,
        "types should run before matcher hook tasks"
    );
    assert!(
        p_gen < p_a_install,
        "matcher hook tasks should run before bun.install"
    );
    assert!(p_a_install < p_dev, "bun.install should run before dev");
}
