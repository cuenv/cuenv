//! `cuenv sync ci` output for the Infisical OIDC contributors.
//!
//! Each test copies an `examples/` project into a scratch repository with the
//! schema and the built-in contributors, generates its GitHub workflow, and
//! checks the steps and permissions that make the pattern work end to end.

use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

const CUENV_BIN: &str = env!("CARGO_BIN_EXE_cuenv");

fn repo_root() -> TestResult<PathBuf> {
    Ok(PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?)
}

fn copy_cue_files(src: &Path, dst: &Path) -> TestResult {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let path = entry?.path();
        let Some(name) = path.file_name() else {
            continue;
        };
        if path.is_dir() {
            copy_cue_files(&path, &dst.join(name))?;
        } else if path.extension().is_some_and(|extension| extension == "cue") {
            fs::copy(&path, dst.join(name))?;
        }
    }
    Ok(())
}

/// Generate the GitHub workflow of `examples/<example>` and return it.
fn generate_workflow(example: &str, pipeline: &str) -> TestResult<String> {
    let repo = repo_root()?;
    let temp = tempfile::Builder::new()
        .prefix("cuenv_infisical_")
        .tempdir()?;
    let root = temp.path();

    fs::create_dir_all(root.join("cue.mod"))?;
    fs::write(
        root.join("cue.mod/module.cue"),
        "module: \"github.com/cuenv/cuenv\"\nlanguage: {\n\tversion: \"v0.14.1\"\n}\n",
    )?;
    copy_cue_files(&repo.join("schema"), &root.join("schema"))?;
    copy_cue_files(
        &repo.join("contrib/contributors"),
        &root.join("contrib/contributors"),
    )?;
    let source = fs::read_to_string(repo.join("examples").join(example).join("env.cue"))?;
    fs::write(
        root.join("env.cue"),
        source.replacen("package examples", "package cuenv", 1),
    )?;

    let git = Command::new("git").arg("init").current_dir(root).output()?;
    assert!(git.status.success(), "git init failed");

    let output = Command::new(CUENV_BIN)
        .args(["sync", "ci"])
        .current_dir(root)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", std::env::var("HOME").unwrap_or_default())
        .output()?;
    assert!(
        output.status.success(),
        "sync ci failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    Ok(fs::read_to_string(root.join(format!(
        ".github/workflows/{example}-{pipeline}.yml"
    )))?)
}

fn position(workflow: &str, needle: &str) -> TestResult<usize> {
    workflow
        .find(needle)
        .ok_or_else(|| format!("workflow should contain {needle:?}:\n{workflow}").into())
}

/// The text of the step whose `name:` line is `step_name`.
fn step_text<'a>(workflow: &'a str, step_name: &str) -> TestResult<&'a str> {
    let start = position(workflow, &format!("- name: {step_name}"))?;
    let rest = &workflow[start + 2..];
    let end = rest.find("\n    - name:").unwrap_or(rest.len());
    Ok(&rest[..end])
}

#[test]
fn infisical_oidc_exports_a_github_id_token_before_cuenv_resolves_secrets() -> TestResult {
    let workflow = generate_workflow("ci-infisical-oidc", "deploy")?;

    assert!(workflow.contains("id-token: write"), "{workflow}");

    let token = position(&workflow, "- name: Get GitHub OIDC token for Infisical")?;
    let setup = position(&workflow, "- name: Setup Infisical")?;
    let cuenv_setup = position(&workflow, "- name: Setup cuenv")?;
    let run = position(&workflow, "- name: 'Run pipeline: deploy'")?;
    assert!(
        cuenv_setup < token && token < setup && setup < run,
        "{workflow}"
    );

    let token_step = step_text(&workflow, "Get GitHub OIDC token for Infisical")?;
    for expected in [
        "ACTIONS_ID_TOKEN_REQUEST_URL",
        "Authorization: Bearer ${ACTIONS_ID_TOKEN_REQUEST_TOKEN}",
        "audience='https://github.com/cuenv'",
        "echo \"::add-mask::${jwt}\"",
        "echo \"INFISICAL_AUTH_JWT=${jwt}\"",
        "echo \"INFISICAL_MACHINE_IDENTITY_ID=${INFISICAL_MACHINE_IDENTITY_ID}\"",
        ">> \"$GITHUB_ENV\"",
        "INFISICAL_MACHINE_IDENTITY_ID: ${{ secrets.INFISICAL_MACHINE_IDENTITY_ID }}",
    ] {
        assert!(token_step.contains(expected), "{expected}:\n{token_step}");
    }
    // The token step runs as plain shell, before cuenv can resolve anything.
    assert!(!token_step.contains("cuenv exec"), "{token_step}");

    let run_step = step_text(&workflow, "'Run pipeline: deploy'")?;
    assert!(
        run_step.contains(
            "INFISICAL_MACHINE_IDENTITY_ID: ${{ secrets.INFISICAL_MACHINE_IDENTITY_ID }}"
        ),
        "{run_step}"
    );
    // The JWT reaches cuenv through $GITHUB_ENV, never as a secret reference.
    assert!(
        !workflow.contains("secrets.INFISICAL_AUTH_JWT"),
        "{workflow}"
    );
    Ok(())
}

#[test]
fn infisical_secrets_action_runs_the_official_action_with_oidc() -> TestResult {
    let workflow = generate_workflow("ci-infisical-action", "deploy")?;

    assert!(workflow.contains("id-token: write"), "{workflow}");

    let action = position(&workflow, "- name: Fetch secrets from Infisical")?;
    let run = position(&workflow, "- name: 'Run pipeline: deploy'")?;
    assert!(action < run, "{workflow}");

    let action_step = step_text(&workflow, "Fetch secrets from Infisical")?;
    for expected in [
        "uses: Infisical/secrets-action@v1.0.18",
        "method: oidc",
        "identity-id: ${{ secrets.INFISICAL_MACHINE_IDENTITY_ID }}",
        "project-slug: my-project",
        "env-slug: prod",
        "secret-path: /deploy",
    ] {
        assert!(action_step.contains(expected), "{expected}:\n{action_step}");
    }
    assert!(!action_step.contains("project-id"), "{action_step}");
    // The action does the login; cuenv's Infisical setup is not involved.
    assert!(!workflow.contains("Setup Infisical"), "{workflow}");
    Ok(())
}
