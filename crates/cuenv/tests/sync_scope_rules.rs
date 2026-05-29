//! Integration tests for sync scope rules (project vs workspace).
//!
//! These tests assert how `cuenv sync` behaves from root vs nested paths and
//! how `-A` affects CI workflow generation.

use std::error::Error;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

const CUENV_BIN: &str = env!("CARGO_BIN_EXE_cuenv");
const EXPECTED_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Create a Command with a clean environment (no CI vars leaking).
fn clean_environment_command(bin: impl AsRef<OsStr>) -> Command {
    let mut cmd = Command::new(bin);
    cmd.env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", std::env::var("HOME").unwrap_or_default())
        .env("USER", std::env::var("USER").unwrap_or_default());
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

fn write_local_cuenv_module(root: &Path) -> TestResult {
    let cue_mod_dir = root.join("cue.mod");
    fs::create_dir_all(&cue_mod_dir)?;
    fs::write(
        cue_mod_dir.join("module.cue"),
        "module: \"github.com/cuenv/cuenv\"\nlanguage: {\n\tversion: \"v0.9.0\"\n}\n",
    )?;

    let schema_src = repo_root()?.join("schema");
    let schema_dst = root.join("schema");
    copy_dir_recursive(&schema_src, &schema_dst)?;

    Ok(())
}

fn write_current_cuenv_marker(root: &Path) -> TestResult {
    fs::write(
        root.join("cue.mod/module.cue"),
        format!(
            r#"module: "github.com/cuenv/cuenv"
language: {{
  version: "v0.9.0"
}}
custom: "github.com/cuenv/cuenv": version: "{EXPECTED_VERSION}"
"#
        ),
    )?;
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

fn create_repo() -> TestResult<TempDir> {
    let temp_dir = tempfile::Builder::new().prefix("cuenv_test_").tempdir()?;
    let root = temp_dir.path();
    write_local_cuenv_module(root)?;
    init_git_repo(root)?;
    Ok(temp_dir)
}

fn project_env_cue(name: &str, pipeline: &str, task: &str, _owner: &str) -> String {
    format!(
        r#"package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {{
  // Alias to avoid scoping conflict with pipeline's tasks field
  let _t = tasks

  name: "{name}"

  ci: {{
    providers: ["github"]
    pipelines: {{
      "{pipeline}": {{
        tasks: [_t.{task}]
      }}
    }}
  }}

  tasks: {{
    {task}: {{
      command: "echo"
      args: ["{task}"]
      inputs: ["env.cue"]
    }}
  }}
}}
"#
    )
}

fn ci_provider_project_env_cue(name: &str, provider: &str) -> String {
    format!(
        r#"package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {{
  let _t = tasks

  name: "{name}"

  ci: {{
    providers: ["{provider}"]
    pipelines: {{
      check: {{
        tasks: [_t.check]
      }}
    }}
  }}

  tasks: {{
    check: schema.#Task & {{
      command: "echo"
      args: ["check"]
      inputs: ["env.cue"]
    }}
  }}
}}
"#
    )
}

fn project_env_cue_with_trigger_inputs(name: &str, pipeline: &str, task: &str) -> String {
    format!(
        r#"package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {{
  let _t = tasks

  name: "{name}"

  ci: {{
    providers: ["github"]
    pipelines: {{
      "{pipeline}": {{
        when: {{
          branch: "main"
          pullRequest: true
        }}
        tasks: [_t.{task}]
      }}
    }}
  }}

  tasks: {{
    {task}: schema.#Task & {{
      command: "echo"
      args: ["{task}"]
      inputs: [
        "../flake.nix",
        "../infrastructure/waddle.cloud/gitops/waddle-server/**",
      ]
    }}
  }}
}}
"#
    )
}

fn base_env_cue(_owner: &str, _include_ignore: bool) -> String {
    r#"package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Base
"#
    .to_string()
}

fn tools_project_env_cue(name: &str) -> String {
    format!(
        r#"package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {{
  name: "{name}"

  runtime: schema.#ToolsRuntime & {{
    platforms: ["darwin-arm64"]
    tools: {{
      rust: {{
        version: "stable"
        source: schema.#Rustup & {{
          toolchain: "stable"
        }}
      }}
    }}
  }}
}}
"#
    )
}

fn nix_runtime_project_env_cue(name: &str) -> String {
    format!(
        r#"package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {{
  name: "{name}"

  runtime: schema.#NixRuntime
}}
"#
    )
}

fn all_rules_cue() -> &'static str {
    r#"package cuenv

import "github.com/cuenv/cuenv/schema/rules"

rules.#DirectoryRules & {
  ignore: {
    git: [
      "target/",
      ".env",
    ]
    docker: [
      "target/",
      ".git/",
    ]
  }

  editorconfig: {
    "*": {
      indent_style: "space"
      indent_size: 2
      end_of_line: "lf"
      insert_final_newline: true
    }
    "*.md": {
      trim_trailing_whitespace: false
    }
  }

  owners: rules: {
    default: {
      pattern: "**"
      owners: ["@cuenv/maintainers"]
      order: 0
    }
    rust: {
      pattern: "*.rs"
      owners: ["@cuenv/rust"]
      description: "Rust files override the fallback owner"
      section: "Language owners"
      order: 10
    }
  }
}
"#
}

fn stale_lockfile() -> &'static str {
    r#"version = 3

[tools.jq]
version = "1.7.1"

[tools.jq.platforms.darwin-arm64]
provider = "github"
digest = "sha256:abc"

[tools.jq.platforms.darwin-arm64.source]
type = "github"
repo = "jqlang/jq"
tag = "jq-1.7.1"
asset = "jq"
"#
}

fn minimal_flake_lock(nar_hash: &str) -> String {
    format!(
        r#"{{
  "nodes": {{
    "nixpkgs": {{
      "locked": {{
        "type": "github",
        "owner": "NixOS",
        "repo": "nixpkgs",
        "rev": "abc123",
        "narHash": "{nar_hash}"
      }},
      "original": {{
        "type": "github",
        "owner": "NixOS",
        "repo": "nixpkgs"
      }}
    }},
    "root": {{
      "inputs": {{
        "nixpkgs": "nixpkgs"
      }}
    }}
  }},
  "root": "root",
  "version": 7
}}"#
    )
}

fn run_cuenv(current_dir: &Path, args: &[&str]) -> TestResult<CuenvOutput> {
    let output = clean_environment_command(CUENV_BIN)
        .args(args)
        .current_dir(current_dir)
        .output()?;

    Ok(CuenvOutput {
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        success: output.status.success(),
    })
}

#[test]
fn sync_root_project_only_generates_root_ci() -> TestResult {
    let tmp = create_repo()?;
    let root = tmp.path();

    fs::write(
        root.join("env.cue"),
        project_env_cue("root", "build", "build", "@root"),
    )?;

    let output = run_cuenv(root, &["sync"])?;
    assert!(output.success, "sync failed: {}", output.stderr);

    let workflows_dir = root.join(".github/workflows");
    assert!(workflows_dir.join("root-build.yml").exists());
    assert!(!workflows_dir.join("service-test.yml").exists());

    Ok(())
}

#[test]
fn sync_nested_project_only_generates_nested_ci_in_repo_root() -> TestResult {
    let tmp = create_repo()?;
    let root = tmp.path();

    fs::write(root.join("env.cue"), base_env_cue("@root", false))?;

    let nested = root.join("apps/service");
    fs::create_dir_all(&nested)?;
    fs::write(
        nested.join("env.cue"),
        project_env_cue("service", "test", "test", "@service"),
    )?;

    let output = run_cuenv(&nested, &["sync"])?;
    assert!(output.success, "sync failed: {}", output.stderr);

    let workflows_dir = root.join(".github/workflows");
    assert!(workflows_dir.join("service-test.yml").exists());
    assert!(!workflows_dir.join("root-build.yml").exists());
    assert!(!nested.join(".github").exists());

    Ok(())
}

#[test]
fn sync_nested_project_normalizes_parent_trigger_paths() -> TestResult {
    let tmp = create_repo()?;
    let root = tmp.path();

    fs::write(root.join("env.cue"), base_env_cue("@root", false))?;
    fs::write(root.join("flake.nix"), "{}")?;
    fs::create_dir_all(root.join("infrastructure/waddle.cloud/gitops/waddle-server"))?;

    let nested = root.join("server");
    fs::create_dir_all(&nested)?;
    fs::write(
        nested.join("env.cue"),
        project_env_cue_with_trigger_inputs("server", "deploy", "deploy"),
    )?;

    let output = run_cuenv(&nested, &["sync"])?;
    assert!(output.success, "sync failed: {}", output.stderr);

    let workflow_path = root.join(".github/workflows/server-deploy.yml");
    let workflow = fs::read_to_string(&workflow_path)?;

    assert!(workflow.contains("flake.nix"), "{workflow}");
    assert!(
        workflow.contains("infrastructure/waddle.cloud/gitops/waddle-server/**"),
        "{workflow}"
    );
    assert!(workflow.contains("server/env.cue"), "{workflow}");
    assert!(!workflow.contains("server/../"), "{workflow}");
    assert!(!workflow.contains("../"), "{workflow}");

    Ok(())
}

#[test]
fn sync_all_from_nested_generates_all_ci_in_repo_root() -> TestResult {
    let tmp = create_repo()?;
    let root = tmp.path();

    fs::write(root.join("env.cue"), base_env_cue("@root", false))?;

    let nested = root.join("apps/service");
    fs::create_dir_all(&nested)?;
    fs::write(
        nested.join("env.cue"),
        project_env_cue("service", "test", "test", "@service"),
    )?;

    let other = root.join("apps/api");
    fs::create_dir_all(&other)?;
    fs::write(
        other.join("env.cue"),
        project_env_cue("api", "build", "build", "@api"),
    )?;

    let output = run_cuenv(&nested, &["sync", "-A"])?;
    assert!(output.success, "sync -A failed: {}", output.stderr);

    let workflows_dir = root.join(".github/workflows");
    assert!(workflows_dir.join("service-test.yml").exists());
    assert!(workflows_dir.join("api-build.yml").exists());
    assert!(!nested.join(".github").exists());

    Ok(())
}

#[test]
fn sync_ci_rejects_schema_only_gitlab_provider() -> TestResult {
    let tmp = create_repo()?;
    let root = tmp.path();

    fs::write(
        root.join("env.cue"),
        ci_provider_project_env_cue("root", "gitlab"),
    )?;

    let output = run_cuenv(root, &["sync", "ci", "--dry-run"])?;
    assert!(
        !output.success,
        "sync ci should reject schema-only GitLab sync\nstdout: {}\nstderr: {}",
        output.stdout, output.stderr
    );

    let combined = format!("{}{}", output.stdout, output.stderr);
    assert!(
        combined.contains("CI sync is not implemented for provider(s): gitlab"),
        "sync ci should explain unsupported GitLab sync: {combined}"
    );
    assert!(!root.join(".gitlab-ci.yml").exists());

    Ok(())
}

#[test]
fn sync_ci_check_fails_when_configured_provider_is_out_of_sync() -> TestResult {
    let tmp = create_repo()?;
    let root = tmp.path();
    write_current_cuenv_marker(root)?;

    fs::write(
        root.join("env.cue"),
        ci_provider_project_env_cue("root", "buildkite"),
    )?;

    let output = run_cuenv(root, &["sync", "ci", "--check"])?;
    assert!(
        !output.success,
        "sync ci --check should fail when configured Buildkite output is missing\nstdout: {}\nstderr: {}",
        output.stdout, output.stderr
    );

    let combined = format!("{}{}", output.stdout, output.stderr);
    assert!(
        combined.contains("Buildkite pipeline.yml missing"),
        "sync ci --check should report missing Buildkite output: {combined}"
    );

    Ok(())
}

#[test]
fn sync_outside_project_errors() -> TestResult {
    let tmp = create_repo()?;
    let root = tmp.path();

    fs::write(root.join("env.cue"), base_env_cue("@root", false))?;

    let nested = root.join("apps/service");
    fs::create_dir_all(&nested)?;
    fs::write(
        nested.join("env.cue"),
        project_env_cue("service", "test", "test", "@service"),
    )?;

    let non_project = root.join("shared");
    fs::create_dir_all(&non_project)?;
    fs::write(non_project.join("env.cue"), base_env_cue("@shared", false))?;

    let output = run_cuenv(&non_project, &["sync"])?;
    assert!(!output.success, "sync should fail outside a project");

    let combined = format!("{}{}", output.stdout, output.stderr);
    assert!(combined.contains("project"));
    assert!(combined.contains("cuenv"));
    assert!(combined.contains("info"));
    assert!(combined.contains("-A"));

    Ok(())
}

#[test]
fn sync_creates_lockfile_for_tools_projects() -> TestResult {
    let tmp = create_repo()?;
    let root = tmp.path();

    fs::write(root.join("env.cue"), tools_project_env_cue("tools-project"))?;

    let output = run_cuenv(root, &["sync"])?;
    assert!(output.success, "sync failed: {}", output.stderr);
    assert!(
        root.join("cuenv.lock").exists(),
        "cuenv sync should create or update cuenv.lock"
    );
    assert!(
        output.stdout.contains("[lock]"),
        "sync output should include the lock provider: {}",
        output.stdout
    );

    Ok(())
}

#[test]
fn sync_all_creates_lockfile_for_tools_projects() -> TestResult {
    let tmp = create_repo()?;
    let root = tmp.path();

    fs::write(root.join("env.cue"), base_env_cue("@root", false))?;

    let nested = root.join("apps/tools");
    fs::create_dir_all(&nested)?;
    fs::write(
        nested.join("env.cue"),
        tools_project_env_cue("tools-project"),
    )?;

    let output = run_cuenv(root, &["sync", "-A"])?;
    assert!(output.success, "sync -A failed: {}", output.stderr);
    assert!(
        root.join("cuenv.lock").exists(),
        "cuenv sync -A should create or update cuenv.lock"
    );
    assert!(
        output.stdout.contains("[lock]"),
        "sync -A output should include the lock provider: {}",
        output.stdout
    );

    Ok(())
}

#[test]
fn sync_creates_runtime_lockfile_for_nix_runtime_projects() -> TestResult {
    let tmp = create_repo()?;
    let root = tmp.path();

    fs::write(
        root.join("env.cue"),
        nix_runtime_project_env_cue("nix-project"),
    )?;
    fs::write(
        root.join("flake.lock"),
        minimal_flake_lock("sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="),
    )?;
    fs::write(root.join("cuenv.lock"), stale_lockfile())?;

    let output = run_cuenv(root, &["sync"])?;
    assert!(output.success, "sync failed: {}", output.stderr);

    let lockfile = fs::read_to_string(root.join("cuenv.lock"))?;
    assert!(
        root.join("cuenv.lock").exists(),
        "cuenv sync should keep cuenv.lock for Nix runtime projects"
    );
    assert!(
        lockfile.contains("[runtimes.\".\"]"),
        "cuenv.lock should contain a root runtime entry: {lockfile}"
    );
    assert!(
        lockfile.contains("type = \"nix\""),
        "cuenv.lock should record the Nix runtime type: {lockfile}"
    );
    assert!(
        lockfile.contains("lockfile = \"flake.lock\""),
        "cuenv.lock should record the flake.lock path: {lockfile}"
    );
    assert!(
        output.stdout.contains("[lock]"),
        "sync output should include the lock provider: {}",
        output.stdout
    );

    Ok(())
}

#[test]
fn sync_check_fails_when_nix_runtime_lockfile_digest_changes() -> TestResult {
    let tmp = create_repo()?;
    let root = tmp.path();

    fs::write(
        root.join("env.cue"),
        nix_runtime_project_env_cue("nix-project"),
    )?;
    fs::write(
        root.join("flake.lock"),
        minimal_flake_lock("sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="),
    )?;

    let output = run_cuenv(root, &["sync"])?;
    assert!(output.success, "initial sync failed: {}", output.stderr);

    fs::write(
        root.join("flake.lock"),
        minimal_flake_lock("sha256-BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB="),
    )?;

    let output = run_cuenv(root, &["sync", "--check"])?;
    assert!(
        !output.success,
        "sync --check should fail after flake.lock changes"
    );

    let output = format!("{}{}", output.stdout, output.stderr);
    assert!(
        output.contains("Lockfile is out of date"),
        "sync --check should report lock drift: {output}"
    );

    Ok(())
}

#[test]
fn sync_check_covers_all_rules_outputs() -> TestResult {
    let tmp = create_repo()?;
    let root = tmp.path();

    fs::write(root.join("env.cue"), base_env_cue("@root", false))?;
    fs::write(root.join(".rules.cue"), all_rules_cue())?;

    let output = run_cuenv(root, &["sync"])?;
    assert!(output.success, "sync failed: {}", output.stderr);

    let gitignore = root.join(".gitignore");
    let dockerignore = root.join(".dockerignore");
    let editorconfig = root.join(".editorconfig");
    let codeowners = root.join(".github/CODEOWNERS");

    assert!(gitignore.exists(), ".gitignore should be generated");
    assert!(dockerignore.exists(), ".dockerignore should be generated");
    assert!(editorconfig.exists(), ".editorconfig should be generated");
    assert!(codeowners.exists(), "CODEOWNERS should be generated");

    let output = run_cuenv(root, &["sync", "--check"])?;
    assert!(
        output.success,
        "sync --check should pass for freshly generated rules outputs\nstdout: {}\nstderr: {}",
        output.stdout, output.stderr
    );

    for (path, expected) in [
        (&gitignore, ".gitignore"),
        (&dockerignore, ".dockerignore"),
        (&editorconfig, ".editorconfig"),
        (&codeowners, "CODEOWNERS"),
    ] {
        let original = fs::read_to_string(path)?;
        fs::write(path, format!("{original}\n# drift\n"))?;

        let output = run_cuenv(root, &["sync", "--check"])?;
        assert!(
            !output.success,
            "sync --check should fail when {expected} drifts"
        );

        let combined = format!("{}{}", output.stdout, output.stderr);
        assert!(
            combined.contains(expected),
            "sync --check should report drift for {expected}: {combined}"
        );

        fs::write(path, original)?;
    }

    Ok(())
}

#[test]
fn rules_codeowners_order_controls_precedence() -> TestResult {
    let tmp = create_repo()?;
    let root = tmp.path();

    fs::write(root.join("env.cue"), base_env_cue("@root", false))?;
    fs::write(root.join(".rules.cue"), all_rules_cue())?;

    let output = run_cuenv(root, &["sync"])?;
    assert!(output.success, "sync failed: {}", output.stderr);

    let codeowners = fs::read_to_string(root.join(".github/CODEOWNERS"))?;
    let fallback = codeowners
        .find("/** @cuenv/maintainers")
        .ok_or_else(|| std::io::Error::other("fallback CODEOWNERS rule missing"))?;
    let rust = codeowners
        .find("/*.rs @cuenv/rust")
        .ok_or_else(|| std::io::Error::other("Rust CODEOWNERS rule missing"))?;

    assert!(
        fallback < rust,
        "broad fallback rule must be emitted before narrower Rust rule so CODEOWNERS precedence works:\n{codeowners}"
    );
    assert!(
        codeowners.contains("# Rust files override the fallback owner"),
        "rule descriptions should be emitted before generated rules:\n{codeowners}"
    );

    Ok(())
}
