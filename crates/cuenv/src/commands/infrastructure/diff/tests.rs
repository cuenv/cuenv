use std::path::Path;
use std::process::Command;

use super::{DiffOptions, execute_diff, render};
use crate::cli::CliError;

type TestResult<Success = ()> = Result<Success, Box<dyn std::error::Error>>;

fn git(repository: &Path, arguments: &[&str]) -> TestResult<String> {
    let output = Command::new("git")
        .current_dir(repository)
        .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
        .args(arguments)
        .output()?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into());
    }
    Ok(String::from_utf8(output.stdout)?)
}

fn commit(repository: &Path) -> TestResult {
    git(repository, &["init", "--quiet"])?;
    git(repository, &["add", "--all"])?;
    git(
        repository,
        &["commit", "--quiet", "--no-gpg-sign", "-m", "base"],
    )?;
    Ok(())
}

/// A CUE module whose project, in `stack`, has these resources and the
/// Cloudflare provider types that `cuenv infra init` generates (and git
/// ignores) for `version`, which `cuenv.lock` pins.
fn project(repository: &Path, version: &str, resources: &str) -> TestResult {
    let digest = format!("sha256:{}", "a".repeat(64));
    let source = "registry.terraform.io/cloudflare/cloudflare";
    for (file, content) in [
        (".gitignore", "cue.mod/gen/\n".to_string()),
        (
            "cue.mod/module.cue",
            "module: \"example.com/stack\"\nlanguage: version: \"v0.14.1\"\n".to_string(),
        ),
        (
            "cue.mod/gen/registry.terraform.io/cloudflare/cloudflare/provider.cue",
            format!(
                "package cloudflare_provider\n#Provider: {{source: \"{source}\", version: \"{version}\"}}\n"
            ),
        ),
        (
            "cuenv.lock",
            format!(
                "version = 5\n[infrastructure_providers.\"{source}\"]\nversion = \"{version}\"\n\
                 schema_digest = \"{digest}\"\nplatforms = {{linux_amd64 = \"{digest}\"}}\n"
            ),
        ),
        (
            "stack/env.cue",
            format!(
                "package cuenv\nimport \"{source}:cloudflare_provider\"\nname: \"stack\"\n\
                 infrastructure: {{\n state: turso: url: \"http://127.0.0.1:8080\"\n providers: \
                 cloudflare: cloudflare_provider.#Provider\n resources: {{\n{resources}\n }}\n}}\n"
            ),
        ),
    ] {
        let path = repository.join(file);
        std::fs::create_dir_all(path.parent().ok_or("a parent directory")?)?;
        std::fs::write(path, content)?;
    }
    Ok(())
}

/// Diff the project with `HEAD`; the base worktree is gone afterwards.
async fn diff(repository: &Path) -> TestResult<Result<String, CliError>> {
    let diff = execute_diff(&DiffOptions {
        path: repository.join("stack").to_string_lossy().into_owned(),
        package: "cuenv".to_string(),
        environment: None,
        base: "HEAD".to_string(),
    })
    .await;
    let worktrees = git(repository, &["worktree", "list", "--porcelain"])?;
    assert_eq!(worktrees.matches("worktree ").count(), 1, "{worktrees}");
    Ok(diff)
}

/// An R2 bucket and a worker that reads `bucket`'s name and a secret that
/// resolving would create the file `secret`, and `other` resources.
fn resources(bucket: &str, secret: &Path, other: &str) -> String {
    format!(
        r#"  assets: {{type: "cloudflare_r2_bucket", configuration: name: "assets", outputs: name: string}}
  api: {{type: "cloudflare_workers_script", configuration: {{
   bucket_name: infrastructure.resources.{bucket}.outputs.name
   secret_text: {{resolver: "exec", command: "touch", args: [{secret:?}]}}
  }}}}
{other}"#
    )
}

#[tokio::test]
async fn shows_the_changed_configuration_and_resolves_no_secret() -> TestResult {
    let directory = tempfile::tempdir()?;
    let (repository, old, new) = (
        directory.path(),
        directory.path().join("old"),
        directory.path().join("new"),
    );
    let legacy = r#"  legacy: {type: "cloudflare_dns_record", configuration: name: "legacy"}"#;
    project(repository, "5.0.0", &resources("assets", &old, legacy))?;
    commit(repository)?;
    let logs = r#"  logs: {type: "cloudflare_r2_bucket", configuration: name: "logs", outputs: name: string}"#;
    project(repository, "5.1.0", &resources("logs", &new, logs))?;
    let rendered = diff(repository).await??;
    for line in [
        "-      \"version\": \"5.0.0\"\n+      \"version\": \"5.1.0\"\n",
        "-        \"bucket_name\": \"${cloudflare_r2_bucket.assets.outputs.name}\",\n\
         +        \"bucket_name\": \"${cloudflare_r2_bucket.logs.outputs.name}\",\n",
        &format!("-            {old:?}\n+            {new:?}\n"),
        "           \"command\": \"touch\",\n",
        "-    \"legacy\": {\n+    \"logs\": {\n",
    ] {
        assert!(rendered.contains(line), "{line} in {rendered}");
    }
    assert!(!old.exists() && !new.exists(), "a secret was resolved");

    commit(repository)?;
    let head = git(repository, &["rev-parse", "--short=7", "HEAD"])?;
    let unchanged = format!(
        "No infrastructure changes compared with HEAD ({}).\n",
        head.trim()
    );
    assert_eq!(diff(repository).await??, unchanged);
    Ok(())
}

#[tokio::test]
async fn the_base_worktree_is_removed_after_an_error() -> TestResult {
    let directory = tempfile::tempdir()?;
    let broken = r#"  broken: {type: "cloudflare_dns_record", configuration: name: string}"#;
    project(directory.path(), "5.1.0", broken)?;
    commit(directory.path())?;
    project(directory.path(), "5.1.0", "")?;
    assert!(diff(directory.path()).await?.is_err());
    Ok(())
}

#[test]
fn a_value_holding_a_fence_cannot_end_the_block() {
    assert!(render("{}\n", "\"```\"\n", "main").starts_with("````diff\n"));
}
