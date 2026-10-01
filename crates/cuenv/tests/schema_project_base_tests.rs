#![allow(missing_docs)]

use cuengine::evaluate_cue_package_typed;
use cuenv_core::manifest::{Base, Project};
use cuenv_core::tasks::TaskDirectoryBase;
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
        "module: \"github.com/cuenv/cuenv\"\nlanguage: {\n\tversion: \"v0.14.1\"\n}\n",
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
fn ordinary_task_discovery_ignores_incomplete_infrastructure_environments() -> TestResult {
    let directory = create_test_dir()?;
    write_local_cuenv_module(directory.path())?;
    fs::write(
        directory.path().join("env.cue"),
        r#"package cuenv
import "github.com/cuenv/cuenv/schema"
schema.#Project
name: "ordinary-task"
tasks: check: {
    command: "sh"
    args: ["-c", "printf 'task ran' > result.txt"]
    hermetic: false
}
infrastructure: {
    state: turso: url: "http://127.0.0.1:1"
    environments: {
        Dev: {
            providers: random: {source: "hashicorp/random", version: "3.9.1"}
            resources: pet: {type: "random_pet", configuration: length: 2}
        }
        Staging: {
            providers: random: {source: string, version: string}
        }
    }
}
"#,
    )?;
    let project = evaluate_cue_package_typed::<Project>(directory.path(), "cuenv")?;
    assert!(project.tasks.contains_key("check"));
    let infrastructure = project.infrastructure.ok_or("missing raw infrastructure")?;
    assert!(infrastructure["environments"]["Staging"]["providers"]["random"]["source"].is_null());
    let output = clean_environment_command(env!("CARGO_BIN_EXE_cuenv"))
        .current_dir(directory.path())
        .args(["task", "--package", "cuenv", "check"])
        .output()?;
    assert!(
        output.status.success(),
        "task discovery failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(directory.path().join("result.txt"))?,
        "task ran"
    );
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
fn named_infrastructure_environments_evaluate_and_decode() -> TestResult {
    let tmp = create_test_dir()?;
    let root = tmp.path();
    write_local_cuenv_module(root)?;

    fs::write(
        root.join("env.cue"),
        r#"package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
  name: "api"
  infrastructure: {
    state: {turso: {url: "http://localhost:8080"}}
    environments: {
      dev: {
        providers: random: {source: "hashicorp/random", version: "3.7.2"}
        resources: {}
      }
      staging: {
        providers: random: {source: "hashicorp/random", path: "bin/provider"}
        resources: {}
      }
    }
  }
}
"#,
    )?;

    let project = evaluate_cue_package_typed::<Project>(root, "cuenv")?;
    let infrastructure: cuenv_core::manifest::Infrastructure = serde_json::from_value(
        project
            .infrastructure
            .ok_or("infrastructure config missing")?,
    )?;
    assert!(infrastructure.environments.contains_key("dev"));
    assert!(infrastructure.environments.contains_key("staging"));
    Ok(())
}

/// A fake 1Password CLI (`whoami` succeeds, `read` prints a fixed secret) so
/// the test resolves a 1Password reference without the real tool.
#[cfg(unix)]
fn write_fake_op(directory: &Path) -> TestResult {
    use std::os::unix::fs::PermissionsExt;

    let script = directory.join("op");
    fs::write(
        &script,
        "#!/bin/sh\ncase \"$1\" in\n  whoami) echo test-user@example.com ;;\n  read) echo fake-onepassword-secret ;;\n  *) exit 2 ;;\nesac\n",
    )?;
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755))?;
    Ok(())
}

const POLICY_ENV: &str = r#"
  PLAIN: "visible-value"
  GUARDED_PLAIN: {value: "guarded-value", policies: [{allowInfrastructure: ["plan"], allowTasks: ["build"]}]}
  EXEC: {resolver: "exec", command: "echo", args: ["exec-secret"]}
  GUARDED_EXEC: {value: {resolver: "exec", command: "echo", args: ["guarded-exec-secret"]}, policies: [{allowInfrastructure: ["apply"]}]}
  OP: {resolver: "onepassword", ref: "op://vault/item/field"}
  GUARDED_OP: {value: {resolver: "onepassword", ref: "op://vault/item/guarded"}, policies: [{allowExec: ["env"], allowInfrastructure: ["plan"]}]}
"#;

#[cfg(unix)]
#[test]
fn documented_policy_form_evaluates_and_resolves_in_the_binary() -> TestResult {
    // `{value: …, policies: […]}` next to plain values, exec secrets and a
    // 1Password reference, with `schema.#Project` embedded at file level and
    // unified with `&`. Before the schema forbade `value` and `policies` in
    // the open `#Secret`, the policy form matched two alternatives of
    // `#EnvironmentVariable` and the project failed to deserialize.
    let bin_directory = create_test_dir()?;
    write_fake_op(bin_directory.path())?;
    let path = format!(
        "{}:{}",
        bin_directory.path().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let sources = [
        format!(
            "package cuenv\n\nimport \"github.com/cuenv/cuenv/schema\"\n\nschema.#Project\n\nname: \"policy-form\"\n\nenv: {{{POLICY_ENV}}}\n"
        ),
        format!(
            "package cuenv\n\nimport \"github.com/cuenv/cuenv/schema\"\n\nschema.#Project & {{\n  name: \"policy-form\"\n  env: {{{POLICY_ENV}}}\n}}\n"
        ),
    ];
    for source in sources {
        let tmp = create_test_dir()?;
        let root = tmp.path();
        write_local_cuenv_module(root)?;
        fs::write(root.join("env.cue"), source)?;

        let project = evaluate_cue_package_typed::<Project>(root, "cuenv")?;
        let env = project.env.ok_or("env missing")?;
        assert_eq!(env.base.len(), 6);

        let output = clean_environment_command(env!("CARGO_BIN_EXE_cuenv"))
            .env("PATH", &path)
            .args(["env", "print", "--path"])
            .arg(root)
            .args(["--package", "cuenv", "--output", "json"])
            .output()?;
        assert!(
            output.status.success(),
            "env print failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let printed: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        assert_eq!(printed["PLAIN"], "visible-value");
        assert_eq!(printed["GUARDED_PLAIN"], "guarded-value");
        // Resolved secrets are redacted in output, but every variable is there.
        for name in ["EXEC", "GUARDED_EXEC", "OP", "GUARDED_OP"] {
            assert!(printed.get(name).is_some(), "{name} missing: {printed}");
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        for secret in [
            "exec-secret",
            "guarded-exec-secret",
            "fake-onepassword-secret",
        ] {
            assert!(!stdout.contains(secret), "{secret} leaked: {stdout}");
        }
    }
    Ok(())
}

#[test]
fn misspelled_policy_field_fails_in_the_binary() -> TestResult {
    let tmp = create_test_dir()?;
    let root = tmp.path();
    write_local_cuenv_module(root)?;
    fs::write(
        root.join("env.cue"),
        r#"package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "typo"

env: TOKEN: {value: "x", policies: [{allowInfrastucture: ["plan"]}]}
"#,
    )?;
    let output = clean_environment_command(env!("CARGO_BIN_EXE_cuenv"))
        .args(["env", "print", "--path"])
        .arg(root)
        .args(["--package", "cuenv"])
        .output()?;
    assert!(!output.status.success(), "the typo must be rejected");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("allowInfrastucture"), "{stderr}");
    Ok(())
}

#[test]
fn schema_checks_fail_every_command_in_every_environment() -> TestResult {
    // A mistake in an environment that no command selects, and one at the top
    // level, are both reported together by a command that has nothing to do
    // with infrastructure.
    let tmp = create_test_dir()?;
    let root = tmp.path();
    write_local_cuenv_module(root)?;
    fs::write(
        root.join("env.cue"),
        r#"package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "checks"

env: PLAIN: "value"

infrastructure: {
  state: turso: url: "http://127.0.0.1:1"
  providers: random: {source: "hashicorp/random", version: "3.7.2"}
  resources: pet: {type: "random_pet", dependsOn: ["nosuch"]}
  environments: staging: {
    providers: random: {source: "hashicorp/random", version: "3.7.2", path: "bin/provider"}
    resources: pet: {type: "random_pet", provider: "missing"}
  }
}
"#,
    )?;
    let output = clean_environment_command(env!("CARGO_BIN_EXE_cuenv"))
        .args(["env", "print", "--path"])
        .arg(root)
        .args(["--package", "cuenv"])
        .output()?;
    assert!(!output.status.success(), "the mistakes must be rejected");
    let stderr = String::from_utf8_lossy(&output.stderr);
    let flattened = stderr.replace(['│', '\n'], " ");
    let flattened = flattened.split_whitespace().collect::<Vec<_>>().join(" ");
    for fragment in [
        "infrastructure._unresolved",
        "no resource named \"nosuch\"",
        "infrastructure.environments.staging._unresolved",
        "no provider named \"missing\"",
        "infrastructure.environments.staging._unresolved.\"providers.random\"",
        "set exactly one of `version` and `path`, not both",
    ] {
        assert!(flattened.contains(fragment), "{fragment} missing: {stderr}");
    }
    Ok(())
}

#[test]
fn task_dir_defaults_to_definition_dot() -> TestResult {
    let tmp = create_test_dir()?;
    let root = tmp.path();
    write_local_cuenv_module(root)?;

    fs::write(
        root.join("env.cue"),
        r#"package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
  name: "app"
  tasks: {
    build: schema.#Task & {
      command: "true"
    }
  }
}
"#,
    )?;

    let project = evaluate_cue_package_typed::<Project>(root, "cuenv")?;
    let task = project
        .tasks
        .get("build")
        .and_then(|node| node.as_task())
        .ok_or_else(|| std::io::Error::other("expected build task"))?;
    let dir = task
        .directory
        .as_ref()
        .ok_or_else(|| std::io::Error::other("expected task dir default"))?;

    assert_eq!(dir.from, TaskDirectoryBase::Definition);
    assert_eq!(dir.path, ".");
    Ok(())
}

#[test]
fn task_dir_rejects_string_form() -> TestResult {
    let tmp = create_test_dir()?;
    let root = tmp.path();
    write_local_cuenv_module(root)?;

    fs::write(
        root.join("env.cue"),
        r#"package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
  name: "app"
  tasks: {
    build: schema.#Task & {
      command: "true"
      dir: "apps/web"
        }
    }
}
"#,
    )?;

    let res = evaluate_cue_package_typed::<Project>(root, "cuenv");
    assert!(res.is_err(), "schema should reject string task dir");
    Ok(())
}

#[test]
fn task_requires_exactly_one_execution_mode() -> TestResult {
    for task_fields in [
        "description: \"missing executable\"",
        "command: \"echo\"\n      script: \"echo\"",
    ] {
        let tmp = create_test_dir()?;
        let root = tmp.path();
        write_local_cuenv_module(root)?;

        let env = r#"package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
  name: "app"
  tasks: {
    invalid: schema.#Task & {
      TASK_FIELDS
    }
  }
}
"#
        .replace("TASK_FIELDS", task_fields);
        fs::write(root.join("env.cue"), env)?;

        let res = evaluate_cue_package_typed::<Project>(root, "cuenv");
        assert!(
            res.is_err(),
            "cuenv should reject task fields: {task_fields}"
        );
    }

    Ok(())
}

#[test]
fn task_group_rejects_invalid_children() -> TestResult {
    let tmp = create_test_dir()?;
    let root = tmp.path();
    write_local_cuenv_module(root)?;

    fs::write(
        root.join("env.cue"),
        r#"package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
  name: "app"
  tasks: {
    checks: schema.#TaskGroup & {
      type: "group"
      invalid: "not a task"
    }
  }
}
"#,
    )?;

    let res = evaluate_cue_package_typed::<Project>(root, "cuenv");
    assert!(
        res.is_err(),
        "schema should reject a non-task child in a task group"
    );
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
fn codegen_accepts_all_schema_file_types() -> TestResult {
    let tmp = create_test_dir()?;
    let root = tmp.path();
    write_local_cuenv_module(root)?;

    fs::write(
        root.join("env.cue"),
        r##"package cuenv

import (
  "github.com/cuenv/cuenv/schema"
  gen "github.com/cuenv/cuenv/schema/codegen"
)

schema.#Project & {
  name: "app"
  codegen: {
    files: {
      "src/app.ts": gen.#TypeScriptFile & {
        content: "export const answer = 42;\n"
        lint: {
          enabled: false
        }
      }
      "src/app.js": gen.#JavaScriptFile & {
        content: "export const answer = 42;\n"
      }
      "package.json": gen.#JSONFile & {
        content: "{\"name\":\"app\"}"
        gitignore: false
        format: {
          indentSize: 4
        }
      }
      "tsconfig.jsonc": gen.#JSONCFile & {
        content: "{ // comment\n  \"compilerOptions\": {}\n}\n"
      }
      "config.yaml": gen.#YAMLFile & {
        content: "name: app\n"
      }
      "Cargo.toml": gen.#TOMLFile & {
        content: "[package]\nname = \"app\"\n"
      }
      "src/main.rs": gen.#RustFile & {
        content: "fn main() {}\n"
      }
      "main.go": gen.#GoFile & {
        content: "package main\n"
      }
      "main.py": gen.#PythonFile & {
        content: "print(\"app\")\n"
      }
      "README.md": gen.#MarkdownFile & {
        content: "# app\n"
      }
      "scripts/run.sh": gen.#ShellScriptFile & {
        content: "#!/usr/bin/env bash\n"
      }
      "Dockerfile": gen.#DockerfileFile & {
        content: "FROM scratch\n"
      }
      "flake.nix": gen.#NixFile & {
        content: "{ }\n"
      }
    }
  }
}
"##,
    )?;

    let project = evaluate_cue_package_typed::<Project>(root, "cuenv")?;
    let codegen = project.codegen.ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "schema should evaluate codegen config",
        )
    })?;

    assert_eq!(codegen.files.len(), 13);
    assert_eq!(codegen.files["src/app.ts"].language, "typescript");
    assert_eq!(codegen.files["src/app.js"].language, "javascript");
    assert_eq!(codegen.files["package.json"].language, "json");
    assert_eq!(codegen.files["tsconfig.jsonc"].language, "jsonc");
    assert_eq!(codegen.files["config.yaml"].language, "yaml");
    assert_eq!(codegen.files["Cargo.toml"].language, "toml");
    assert_eq!(codegen.files["src/main.rs"].language, "rust");
    assert_eq!(codegen.files["main.go"].language, "go");
    assert_eq!(codegen.files["main.py"].language, "python");
    assert_eq!(codegen.files["README.md"].language, "markdown");
    assert_eq!(codegen.files["scripts/run.sh"].language, "shell");
    assert_eq!(codegen.files["Dockerfile"].language, "dockerfile");
    assert_eq!(codegen.files["flake.nix"].language, "nix");
    assert_eq!(codegen.files["package.json"].format.indent_size, Some(4));
    assert_eq!(
        codegen.files["src/app.ts"]
            .lint
            .as_ref()
            .map(|lint| lint.enabled),
        Some(false)
    );
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
