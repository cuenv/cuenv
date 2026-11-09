//! Exercise generated shell commands with real interpreters.

use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

struct Fixture {
    directory: TempDir,
    state: PathBuf,
    session: uuid::Uuid,
}

impl Fixture {
    fn new() -> TestResult<Self> {
        let directory = tempfile::Builder::new().prefix("cuenv_‘’‚‛_").tempdir()?;
        let state = directory.path().join("state");
        fs::create_dir(&state)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&state, fs::Permissions::from_mode(0o700))?;
        }
        fs::create_dir(directory.path().join("cue.mod"))?;
        fs::write(
            directory.path().join("cue.mod/module.cue"),
            "module: \"test.example/shell-security\"\nlanguage: version: \"v0.9.0\"\n",
        )?;
        Ok(Self {
            directory,
            state,
            session: uuid::Uuid::new_v4(),
        })
    }

    fn path(&self) -> &Path {
        self.directory.path()
    }

    fn project(&self, name: &str, value: &str) -> TestResult {
        let name = serde_json::to_string(name)?;
        let value = serde_json::to_string(value)?;
        fs::write(
            self.path().join("env.cue"),
            format!("package cuenv\nname: \"test\"\nenv: {{{name}: {value}}}\n"),
        )?;
        Ok(())
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_cuenv"));
        command
            .current_dir(self.path())
            .env("HOME", self.path())
            .env("XDG_STATE_HOME", &self.state)
            .env("XDG_CACHE_HOME", self.path())
            .env("CUENV_STATE_DIR", &self.state)
            .env("CUENV_APPROVAL_FILE", self.path().join("approvals.json"))
            .env("CUENV_EXECUTABLE", env!("CARGO_BIN_EXE_cuenv"))
            .env("CUENV_HOOK_SESSION", self.session.to_string())
            .env_remove("CUENV_MANAGED_VARS")
            .env_remove("CUENV_LOADED_DIR")
            .env_remove("CUENV_PENDING_APPROVAL_DIR");
        for variable in [
            "CI",
            "GITHUB_ACTIONS",
            "GITLAB_CI",
            "BUILDKITE",
            "JENKINS_URL",
            "CIRCLECI",
            "TRAVIS",
            "BITBUCKET_PIPELINES",
            "AZURE_PIPELINES",
            "TF_BUILD",
            "DRONE",
            "TEAMCITY_VERSION",
        ] {
            command.env_remove(variable);
        }
        command
    }

    fn export(&self, shell: &str) -> TestResult<Output> {
        Ok(self.command().args(["export", "--shell", shell]).output()?)
    }
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "command failed: stdout={}, stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn session_ids_are_fresh_without_a_project_or_hook_state() -> TestResult {
    let directory = tempfile::tempdir()?;
    let state = directory.path().join("uncreated-state");
    let session = || {
        Command::new(env!("CARGO_BIN_EXE_cuenv"))
            .current_dir(directory.path())
            .env("HOME", directory.path())
            .env("XDG_STATE_HOME", &state)
            .env("CUENV_STATE_DIR", &state)
            .args(["shell", "session"])
            .output()
    };
    let first = session()?;
    let second = session()?;
    assert_success(&first);
    assert_success(&second);
    let first = String::from_utf8(first.stdout)?.trim().to_string();
    let second = String::from_utf8(second.stdout)?.trim().to_string();
    let id = uuid::Uuid::parse_str(&first)?;
    assert_eq!(id.to_string(), first);
    assert_eq!(id.get_version_num(), 4);
    assert!(!id.is_nil());
    assert_ne!(first, second);
    assert!(
        !state.exists(),
        "generating an ID must not create hook state"
    );
    Ok(())
}

#[test]
fn powershell_export_keeps_quotes_and_expressions_literal() -> TestResult {
    let fixture = Fixture::new()?;
    let sentinel = fixture.path().join("injected");
    let payload = format!(
        "a 'quote' ‘left’ ‚low‛ and $([System.IO.File]::WriteAllText('{}','injected'))",
        sentinel.display()
    );
    fixture.project("VALUE", &payload)?;
    let exported = fixture.export("powershell")?;
    assert_success(&exported);
    let script = format!(
        "{}\n[Console]::Out.Write($env:VALUE)",
        String::from_utf8(exported.stdout)?
    );
    let interpreter = std::env::var_os("CUENV_TEST_PWSH")
        .ok_or("CUENV_TEST_PWSH must name the pinned PowerShell interpreter")?;
    let output = Command::new(interpreter)
        .env("HOME", fixture.path())
        .env("XDG_CACHE_HOME", fixture.path())
        .env("XDG_CONFIG_HOME", fixture.path())
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output()?;
    assert_success(&output);
    assert!(String::from_utf8(output.stdout)?.ends_with(&payload));
    assert!(
        !sentinel.exists(),
        "the literal expression must not execute"
    );
    Ok(())
}

#[test]
fn powershell_preserves_empty_values_and_rejects_older_versions_before_mutation() -> TestResult {
    let fixture = Fixture::new()?;
    fixture.project("VALUE", "")?;
    let exported = fixture.export("powershell")?;
    assert_success(&exported);
    let script = String::from_utf8(exported.stdout)?;
    let interpreter = std::env::var_os("CUENV_TEST_PWSH")
        .ok_or("CUENV_TEST_PWSH must name the pinned PowerShell interpreter")?;
    let run = |script: &str| {
        Command::new(&interpreter)
            .env("HOME", fixture.path())
            .env("XDG_CACHE_HOME", fixture.path())
            .env("XDG_CONFIG_HOME", fixture.path())
            .args(["-NoProfile", "-NonInteractive", "-Command", script])
            .output()
    };
    let current = run(&format!(
        "{script}\nif (Test-Path Env:VALUE) {{ [Console]::Out.Write('present:' + $env:VALUE.Length) }}"
    ))?;
    assert_success(&current);
    assert!(String::from_utf8(current.stdout)?.ends_with("present:0"));
    let older = run(&format!(
        "$PSVersionTable.PSVersion = [version]'7.4'; $env:VALUE = 'original'; try {{ {script} }} catch {{ [Console]::Out.Write('blocked:' + $env:VALUE) }}"
    ))?;
    assert_success(&older);
    assert_eq!(String::from_utf8(older.stdout)?, "blocked:original");
    Ok(())
}

#[test]
fn fish_export_round_trips_literals_after_the_function_returns() -> TestResult {
    let fixture = Fixture::new()?;
    let value = "`literal` back\\slash $dollar \"quote\" 'apostrophe'\nnext line";
    fixture.project("CUENV_SHELL_PROBE", value)?;
    let exported = fixture.export("fish")?;
    assert_success(&exported);
    let script = format!(
        "function apply_export\n{}\nend\napply_export\nprintf '%s' \"$CUENV_SHELL_PROBE\"",
        String::from_utf8(exported.stdout)?
    );
    let output = Command::new("fish").args(["-c", &script]).output()?;
    assert_success(&output);
    assert_eq!(String::from_utf8(output.stdout)?, value);
    Ok(())
}

#[test]
fn export_rejects_a_shell_command_as_a_variable_name() -> TestResult {
    let fixture = Fixture::new()?;
    let sentinel = fixture.path().join("injected");
    fixture.project(&format!("X;touch {}", sentinel.display()), "value")?;
    let output = fixture.export("bash")?;
    assert!(!output.status.success());
    assert!(output.stdout.is_empty(), "no shell script may be emitted");
    assert!(!sentinel.exists());
    Ok(())
}

#[test]
fn env_check_validates_names_policies_and_approval_after_a_source_hook() -> TestResult {
    let fixture = Fixture::new()?;
    let sentinel = fixture.path().join("injected");
    let hook = format!(
        "printf '\\000__CUENV_ENV_START__\\000X;touch {}=value\\000'\nexport HOOK_VALUE=from_hook",
        sentinel.display()
    );
    let hook = serde_json::to_string(&hook)?;
    let bad_name = serde_json::to_string(&format!("X;touch {}", sentinel.display()))?;
    let project = |extra: &str| {
        format!(
            r#"package cuenv
name: "test"
env: {{
    PUBLIC_VALUE: "visible"
    RESTRICTED_VALUE: {{value: "not-for-shell", policies: [{{allowExec: ["env"]}}]}}
    {extra}
}}
hooks: onEnter: probe: {{command: "printf", args: ["%s\n", {hook}], source: true}}
"#
        )
    };
    fs::write(
        fixture.path().join("env.cue"),
        project(&format!("{bad_name}: \"value\"")),
    )?;
    let unapproved = fixture.command().args(["env", "check"]).output()?;
    assert!(
        !unapproved.status.success(),
        "unapproved hook results must not be exported"
    );
    assert!(unapproved.stdout.is_empty());

    let allow = fixture.command().args(["allow", "--yes"]).output()?;
    assert_success(&allow);
    let load = fixture.command().args(["env", "load"]).output()?;
    assert_success(&load);
    let status = fixture
        .command()
        .args([
            "env",
            "status",
            "--wait",
            "--timeout",
            "30",
            "--output",
            "short",
        ])
        .output()?;
    assert_success(&status);
    assert_eq!(String::from_utf8_lossy(&status.stdout).trim(), "[OK]");
    let inspect = fixture.command().args(["env", "inspect"]).output()?;
    assert_success(&inspect);
    let inspection = String::from_utf8(inspect.stdout)?;
    assert!(inspection.contains("Captured env:"));
    assert!(!inspection.contains("from_hook"));

    let rejected = fixture
        .command()
        .args(["env", "check", "--shell", "bash"])
        .output()?;
    assert!(!rejected.status.success());
    assert!(
        rejected.stdout.is_empty(),
        "invalid keys must emit no shell script"
    );
    assert!(!sentinel.exists());

    fs::write(fixture.path().join("env.cue"), project(""))?;
    let checked = fixture
        .command()
        .args(["env", "check", "--shell", "bash"])
        .output()?;
    assert_success(&checked);
    let script = format!(
        "{}\nprintf '%s:%s:%s' \"$HOOK_VALUE\" \"$PUBLIC_VALUE\" \"${{RESTRICTED_VALUE-unset}}\"",
        String::from_utf8(checked.stdout)?
    );
    let applied = Command::new("bash").args(["-c", &script]).output()?;
    assert_success(&applied);
    assert_eq!(
        String::from_utf8(applied.stdout)?,
        "from_hook:visible:unset"
    );
    assert!(
        !sentinel.exists(),
        "source output must not forge environment capture"
    );

    let deny = fixture.command().args(["deny"]).output()?;
    assert_success(&deny);
    let revoked = fixture.command().args(["env", "check"]).output()?;
    assert!(!revoked.status.success());
    assert!(revoked.stdout.is_empty());
    Ok(())
}
