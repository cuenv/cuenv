//! `cuenv sync ci` output for the Infisical OIDC contributors.
//!
//! Each test copies an `examples/` project into a scratch repository with the
//! schema and the built-in contributors, generates its GitHub workflow, and
//! checks the steps and permissions that make the pattern work end to end.

use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

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

/// Run `cuenv sync ci` on a copy of `examples/<example>` whose `env.cue` is
/// rewritten by `edit`; returns the command output and the scratch repository.
fn sync_example(
    example: &str,
    edit: impl Fn(&str) -> String,
) -> TestResult<(Output, tempfile::TempDir)> {
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
    let source = edit(&source.replacen("package examples", "package cuenv", 1));
    fs::write(root.join("env.cue"), source)?;

    let git = Command::new("git").arg("init").current_dir(root).output()?;
    assert!(git.status.success(), "git init failed");

    let output = Command::new(CUENV_BIN)
        .args(["sync", "ci"])
        .current_dir(root)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", std::env::var("HOME").unwrap_or_default())
        .output()?;
    Ok((output, temp))
}

/// Generate the GitHub workflow of `examples/<example>` and return it.
fn generate_workflow(example: &str, pipeline: &str) -> TestResult<String> {
    let (output, temp) = sync_example(example, str::to_string)?;
    assert!(
        output.status.success(),
        "sync ci failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    Ok(fs::read_to_string(temp.path().join(format!(
        ".github/workflows/{example}-{pipeline}.yml"
    )))?)
}

/// `cuenv sync ci` on an edited example must fail; returns its stderr.
fn sync_failure(example: &str, edit: impl Fn(&str) -> String) -> TestResult<String> {
    let (output, _temp) = sync_example(example, edit)?;
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(!output.status.success(), "sync ci should fail:\n{stderr}");
    Ok(stderr)
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
        "has_line_break \"$INFISICAL_MACHINE_IDENTITY_ID\"",
        "has_line_break \"$jwt\"",
        // Shell escapes, not raw line breaks, in the generated script.
        "tr -d '\\r\\n'",
        "delimiter=\"ghadelimiter_$(od -An -N16 -tx1 /dev/urandom",
        "printf 'INFISICAL_AUTH_JWT<<%s\\n%s\\n%s\\n' \"$delimiter\" \"$jwt\" \"$delimiter\"",
        "printf 'INFISICAL_MACHINE_IDENTITY_ID<<%s\\n%s\\n%s\\n' \"$delimiter\" \"$INFISICAL_MACHINE_IDENTITY_ID\" \"$delimiter\"",
        ">> \"$GITHUB_ENV\"",
        "INFISICAL_MACHINE_IDENTITY_ID: ${{ secrets.INFISICAL_MACHINE_IDENTITY_ID }}",
    ] {
        assert!(token_step.contains(expected), "{expected}:\n{token_step}");
    }
    // The token step runs as plain shell, before cuenv can resolve anything.
    assert!(!token_step.contains("cuenv exec"), "{token_step}");
    // Values only reach $GITHUB_ENV through the heredoc form.
    assert!(!token_step.contains("INFISICAL_AUTH_JWT="), "{token_step}");
    assert!(
        !token_step.contains("INFISICAL_MACHINE_IDENTITY_ID="),
        "{token_step}"
    );

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

/// The `run` script of the step named `step_name`.
fn step_script(workflow: &str, step_name: &str) -> TestResult<String> {
    let yaml: serde_yaml::Value = serde_yaml::from_str(workflow)?;
    let jobs = yaml
        .get("jobs")
        .and_then(serde_yaml::Value::as_mapping)
        .ok_or("workflow has no jobs")?;
    jobs.values()
        .filter_map(|job| job.get("steps")?.as_sequence())
        .flatten()
        .find(|step| step.get("name").and_then(serde_yaml::Value::as_str) == Some(step_name))
        .and_then(|step| step.get("run")?.as_str())
        .map(str::to_string)
        .ok_or_else(|| format!("no step {step_name:?}").into())
}

struct TokenStepRun {
    success: bool,
    stderr: String,
    github_env: String,
}

/// Run the OIDC token step's script with stand-ins for `curl` (the GitHub
/// token endpoint) and `jq` (which returns `jwt` as the token).
fn run_token_step(script: &str, identity_id: &str, jwt: &str) -> TestResult<TokenStepRun> {
    let temp = tempfile::Builder::new()
        .prefix("cuenv_infisical_step_")
        .tempdir()?;
    let bin = temp.path().join("bin");
    fs::create_dir_all(&bin)?;
    let write_tool = |name: &str, body: &str| -> TestResult {
        let path = bin.join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}\n"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755))?;
        }
        Ok(())
    };
    write_tool("curl", "echo '{}'")?;
    // `jq -rn --arg value V '$value|@uri'` echoes V; `jq -r .value` prints
    // the fake token.
    write_tool(
        "jq",
        "if [ \"$1\" = -rn ]; then printf '%s\\n' \"$4\"; else cat >/dev/null; printf '%s\\n' \"$FAKE_JWT\"; fi",
    )?;
    let github_env = temp.path().join("github_env");
    fs::write(&github_env, "")?;

    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let output = Command::new("sh")
        .arg("-c")
        .arg(script)
        .env_clear()
        .env("PATH", path)
        .env("ACTIONS_ID_TOKEN_REQUEST_URL", "https://token.example/?x=1")
        .env("ACTIONS_ID_TOKEN_REQUEST_TOKEN", "request-token")
        .env("INFISICAL_MACHINE_IDENTITY_ID", identity_id)
        .env("FAKE_JWT", jwt)
        .env("GITHUB_ENV", &github_env)
        .output()?;
    Ok(TokenStepRun {
        success: output.status.success(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        github_env: fs::read_to_string(&github_env)?,
    })
}

/// Parse `$GITHUB_ENV` the way GitHub does for `NAME<<DELIMITER` entries.
fn parse_github_env(content: &str) -> TestResult<Vec<(String, String)>> {
    let mut lines = content.lines();
    let mut entries = Vec::new();
    while let Some(line) = lines.next() {
        let (name, delimiter) = line
            .split_once("<<")
            .ok_or_else(|| format!("not a heredoc entry: {line:?}"))?;
        let mut value = Vec::new();
        loop {
            let next = lines.next().ok_or("unterminated heredoc")?;
            if next == delimiter {
                break;
            }
            value.push(next);
        }
        entries.push((name.to_string(), value.join("\n")));
    }
    Ok(entries)
}

#[test]
fn infisical_oidc_token_step_writes_heredoc_entries_with_a_random_delimiter() -> TestResult {
    let workflow = generate_workflow("ci-infisical-oidc", "deploy")?;
    let script = step_script(&workflow, "Get GitHub OIDC token for Infisical")?;

    let first = run_token_step(&script, "identity-id", "header.payload.signature")?;
    assert!(first.success, "{}", first.stderr);
    assert_eq!(
        parse_github_env(&first.github_env)?,
        vec![
            (
                "INFISICAL_AUTH_JWT".to_string(),
                "header.payload.signature".to_string()
            ),
            (
                "INFISICAL_MACHINE_IDENTITY_ID".to_string(),
                "identity-id".to_string()
            ),
        ]
    );
    let delimiter = |run: &TokenStepRun| {
        run.github_env
            .lines()
            .next()
            .and_then(|line| line.split_once("<<"))
            .map(|(_, delimiter)| delimiter.to_string())
            .unwrap_or_default()
    };
    let second = run_token_step(&script, "identity-id", "header.payload.signature")?;
    assert!(delimiter(&first).starts_with("ghadelimiter_"));
    assert!(delimiter(&first).len() > "ghadelimiter_".len() + 16);
    assert_ne!(delimiter(&first), delimiter(&second));
    Ok(())
}

#[test]
fn infisical_oidc_token_step_rejects_line_breaks_without_printing_them() -> TestResult {
    let workflow = generate_workflow("ci-infisical-oidc", "deploy")?;
    let script = step_script(&workflow, "Get GitHub OIDC token for Infisical")?;

    for (identity_id, jwt, variable) in [
        (
            "identity\nINJECTED=1",
            "a.b.c",
            "INFISICAL_MACHINE_IDENTITY_ID",
        ),
        (
            "identity\rINJECTED=1",
            "a.b.c",
            "INFISICAL_MACHINE_IDENTITY_ID",
        ),
        ("identity\n", "a.b.c", "INFISICAL_MACHINE_IDENTITY_ID"),
        ("identity", "a.b.c\nINJECTED=1", "OIDC token"),
    ] {
        let run = run_token_step(&script, identity_id, jwt)?;
        assert!(!run.success, "{identity_id:?} {jwt:?} should fail");
        assert!(run.stderr.contains(variable), "{}", run.stderr);
        assert!(run.stderr.contains("line break"), "{}", run.stderr);
        assert!(!run.stderr.contains("INJECTED"), "{}", run.stderr);
        assert!(run.github_env.is_empty(), "{}", run.github_env);
    }
    Ok(())
}

#[test]
fn infisical_oidc_rejects_a_literal_identity_id_with_a_line_break() -> TestResult {
    let stderr = sync_failure("ci-infisical-oidc", |source| {
        source.replacen(
            "#audience: \"https://github.com/cuenv\"}",
            "#audience: \"https://github.com/cuenv\", #identityId: \"abc\\nX=1\"}",
            1,
        )
    })?;
    assert!(stderr.contains("identityId"), "{stderr}");
    Ok(())
}

#[test]
fn infisical_secrets_action_rejects_both_project_slug_and_id() -> TestResult {
    let stderr = sync_failure("ci-infisical-action", |source| {
        source.replacen(
            "#project: slug: \"my-project\"",
            "#project: {slug: \"my-project\", id: \"00000000-0000-0000-0000-000000000000\"}",
            1,
        )
    })?;
    assert!(stderr.contains("#project"), "{stderr}");
    Ok(())
}
