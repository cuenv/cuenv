//! End-to-end tests for shell-local environment restoration.

#![cfg(unix)]

use std::error::Error;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;
const BASELINE: &str = "cuenv_shell_private_baseline_91cf";

struct Fixture {
    root: TempDir,
    project: PathBuf,
    other: PathBuf,
    bin: PathBuf,
    state: PathBuf,
}

impl Fixture {
    fn new() -> TestResult<Self> {
        let root = tempfile::tempdir()?;
        let project = root.path().join("a");
        let other = root.path().join("b");
        let bin = root.path().join("bin");
        let state = root.path().join("state");
        for path in [&project, &other] {
            fs::create_dir_all(path.join("cue.mod"))?;
            fs::write(
                path.join("cue.mod/module.cue"),
                "module: \"test.example/shell-lifecycle\"\nlanguage: version: \"v0.9.0\"\n",
            )?;
        }
        fs::write(
            project.join("env.cue"),
            "package cuenv\nname: \"a\"\nenv: {KEEP: \"overlay\", REMOVE: \"project\", PATH: \"/cuenv-unavailable-on-this-path\"}\n",
        )?;
        fs::write(
            other.join("env.cue"),
            "package cuenv\nname: \"b\"\nhooks: onEnter: test: {command: \"sh\", args: [\"-c\", \"touch \\\"$TEST_SENTINEL\\\"\"]}\n",
        )?;
        fs::create_dir(&bin)?;
        symlink(env!("CARGO_BIN_EXE_cuenv"), bin.join("cuenv"))?;
        fs::create_dir(&state)?;
        fs::set_permissions(&state, fs::Permissions::from_mode(0o700))?;
        Ok(Self {
            root,
            project,
            other,
            bin,
            state,
        })
    }

    fn command(&self, shell: &str, script: &str) -> TestResult<Output> {
        let mut paths = vec![self.bin.clone()];
        paths.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        let path = std::env::join_paths(paths)?;
        let mut command = Command::new(shell);
        command
            .current_dir(&self.project)
            .env("HOME", self.root.path())
            .env("XDG_STATE_HOME", &self.state)
            .env("XDG_CONFIG_HOME", self.root.path())
            .env("XDG_CACHE_HOME", self.root.path())
            .env("CUENV_STATE_DIR", &self.state)
            .env(
                "CUENV_APPROVAL_FILE",
                self.root.path().join("approvals.json"),
            )
            .env("CUENV_EXECUTABLE", env!("CARGO_BIN_EXE_cuenv"))
            .env("TEST_CUENV", env!("CARGO_BIN_EXE_cuenv"))
            .env("TEST_PROJECT", &self.project)
            .env("TEST_OTHER", &self.other)
            .env("TEST_ROOT", self.root.path())
            .env("TEST_SENTINEL", self.root.path().join("ran-unapproved"))
            .env("TEST_BASELINE", BASELINE)
            .env("TEST_ENV", interpreter("env")?)
            .env("TEST_GREP", interpreter("grep")?)
            .env(
                "TEST_KIND",
                Path::new(shell).file_name().ok_or("invalid shell path")?,
            )
            .env("TEST_SHELL", shell)
            .env("PATH", path)
            .env_remove("CUENV_HOOK_SESSION")
            .env_remove("CUENV_MANAGED_VARS")
            .env_remove("CUENV_LOADED_DIR")
            .env_remove("CUENV_PENDING_APPROVAL_DIR")
            .env_remove("CUENV_SHELL_INTEGRATION")
            .env_remove("CI")
            .env_remove("GITHUB_ACTIONS")
            .env_remove("GITLAB_CI");
        for name in [
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
            command.env_remove(name);
        }
        Ok(command.args(["-c", script]).output()?)
    }

    fn assert_success(&self, output: &Output) {
        assert!(
            output.status.success(),
            "shell failed: status={}, stdout={}, stderr={}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!self.root.path().join("ran-unapproved").exists());
        assert!(
            !String::from_utf8_lossy(&output.stderr).contains(BASELINE),
            "tracing exposed a shell-local baseline"
        );
        assert!(
            !String::from_utf8_lossy(&output.stderr).contains("cuenv_fish_trace_private_3e8a"),
            "a project variable enabled tracing of another value"
        );
    }
}

fn write_input_hook(project: &Path) -> TestResult {
    let source = r#"[ -z "${CUENV_HOOK_SESSION+x}" ] || exit 77
IFS= read -r version < input.txt
if [ "$version" = 1 ]; then
    printf '%s\n' 'export PATH="/cuenv-one:$PATH"' 'export OLD=first'
else
    printf '%s\n' 'export PATH="/cuenv-two:$PATH"'
fi"#;
    fs::write(project.join("input.txt"), "1\n")?;
    fs::write(
        project.join("env.cue"),
        format!(
            "package cuenv\nname: \"a\"\nhooks: onEnter: setup: {{command: \"sh\", args: [\"-c\", {}], inputs: [\"input.txt\"], source: true}}\n",
            serde_json::to_string(source)?
        ),
    )?;
    Ok(())
}

fn interpreter(name: &str) -> TestResult<PathBuf> {
    let path = std::env::var_os("PATH").ok_or("PATH is unavailable")?;
    std::env::split_paths(&path)
        .map(|directory| directory.join(name))
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| format!("{name} is required for the shell integration test").into())
}

#[test]
fn bash_and_zsh_restore_private_baselines_and_reject_partial_updates() -> TestResult {
    for shell in ["bash", "zsh"] {
        let fixture = Fixture::new()?;
        let executable = interpreter(shell)?;
        let script = r#"
export KEEP="$TEST_BASELINE" REMOVE=ambient
unset TEST_BASELINE
export ORIGINAL_PATH="$PATH"
readonly LOCKED=original
source <("$TEST_CUENV" shell init "$TEST_KIND") || exit 10
cuenv shell refresh || exit 31
[ "$KEEP" = overlay ] && [ "$REMOVE" = project ] && [ "$PATH" = /cuenv-unavailable-on-this-path ] || exit 11
"$TEST_SHELL" -c '[ -z "${CUENV_HOOK_SESSION+x}" ] && [ -z "${__cuenv_session+x}" ]' || exit 12
if "$TEST_ENV" | "$TEST_GREP" -q cuenv_shell_private_baseline_91cf; then exit 23; fi
old_session=$(cuenv shell session --current)
source <("$TEST_CUENV" shell init "$TEST_KIND") || exit 24
[ "$(cuenv shell session --current)" = "$old_session" ] || exit 25
cuenv exec -- "$TEST_SHELL" -c '[ -z "${CUENV_HOOK_SESSION+x}" ]' || exit 26
if ( cuenv env status >/dev/null ); then exit 27; fi
( CUENV_HOOK_SESSION="$(cuenv shell session --current)" cuenv env status >/dev/null ) || exit 28
if ( cuenv shell refresh ); then exit 29; fi
if [ "$TEST_KIND" = bash ]; then
    eval "$(CUENV_HOOK_SESSION="$(cuenv shell session --current)" cuenv env check)" || exit 30
else
    cuenv shell refresh || exit 30
fi
PATH="$ORIGINAL_PATH" "$TEST_SHELL" -c 'cd "$TEST_OTHER"; source <("$TEST_CUENV" shell init "$TEST_KIND") || :; cuenv shell refresh || :; [ "$KEEP" = overlay ] && [ "$PATH" = "$ORIGINAL_PATH" ]' || exit 20
[ "$KEEP" = overlay ] && [ "$PATH" = /cuenv-unavailable-on-this-path ] || exit 21
printf '%s\n' 'package cuenv' 'name: "a"' 'env: {KEEP: "broken", LOCKED: "changed"}' > "$TEST_PROJECT/env.cue"
if cuenv shell refresh; then exit 13; fi
[ "$KEEP" = overlay ] && [ "$REMOVE" = project ] || exit 14
printf '%s\n' 'package cuenv' 'name: "a"' 'env: {KEEP: "next", PATH: "/cuenv-unavailable-on-this-path"}' > "$TEST_PROJECT/env.cue"
set -x
cuenv shell refresh || exit 15
case $- in *x*) ;; *) exit 22 ;; esac
set +x
[ "$KEEP" = next ] && [ "$REMOVE" = ambient ] || exit 16
KEEP=user_change
 cd "$TEST_OTHER" || exit 17
cuenv shell refresh || exit 18
[ "$KEEP" = user_change ] && [ "$REMOVE" = ambient ] && [ "$PATH" = "$ORIGINAL_PATH" ] || exit 19
"#;
        let output = fixture.command(
            executable.to_str().ok_or("shell path is not UTF-8")?,
            script,
        )?;
        fixture.assert_success(&output);
    }
    Ok(())
}

#[test]
fn fish_restores_path_lists_and_discards_a_failed_partial_producer() -> TestResult {
    let fixture = Fixture::new()?;
    let script = r#"
set -gx KEEP "$TEST_BASELINE"
set -e -g TEST_BASELINE
set -g -u REMOVE ambient
set -l original_path $PATH
"$TEST_CUENV" shell init fish | source; or exit 10
test "$KEEP" = overlay; and test "$REMOVE" = project; and test "$PATH" = /cuenv-unavailable-on-this-path; or exit 11
if "$TEST_ENV" | "$TEST_GREP" -q cuenv_shell_private_baseline_91cf; exit 21; end
set -l old_session (cuenv shell session --current)
"$TEST_CUENV" shell init fish | source; or exit 22
test (cuenv shell session --current) = "$old_session"; or exit 23
cuenv exec -- "$TEST_SHELL" -c 'not set -q -x CUENV_HOOK_SESSION'; or exit 24
printf '%s\n' 'package cuenv' 'name: "a"' 'env: {KEEP: "next", PATH: "/cuenv-unavailable-on-this-path"}' > "$TEST_PROJECT/env.cue"
set -g fish_trace 1
cuenv shell refresh; or exit 12
test "$fish_trace" = 1; or exit 20
set -e -g fish_trace
test "$KEEP" = next; and test "$REMOVE" = ambient; and not set -q -x REMOVE; or exit 13
set -gx KEEP user_change
cd "$TEST_OTHER"; or exit 14
test "$KEEP" = user_change; and test "$REMOVE" = ambient; or exit 15
test (count $PATH) = (count $original_path); or exit 16
for i in (seq (count $PATH))
    test "$PATH[$i]" = "$original_path[$i]"; or exit 17
end
function __cuenv_test_failed_producer
    printf '%s\n' 'set -gx EXPLOIT injected'
    return 7
end
set -g -u __cuenv_binary __cuenv_test_failed_producer
set -gx EXPLOIT original
if __cuenv_hook; exit 18; end
test "$EXPLOIT" = original; or exit 19
"#;
    let fish = interpreter("fish")?;
    let output = fixture.command(fish.to_str().ok_or("shell path is not UTF-8")?, script)?;
    fixture.assert_success(&output);
    Ok(())
}

#[test]
fn shell_initialization_refuses_existing_cuenv_aliases_and_functions() -> TestResult {
    for shell in ["bash", "zsh", "fish"] {
        let fixture = Fixture::new()?;
        let executable = interpreter(shell)?;
        let script = if shell == "fish" {
            r#"
function cuenv
    printf user-function
end
if "$TEST_CUENV" shell init fish | source; exit 1; end
test (cuenv) = user-function; or exit 2
"#
        } else {
            r#"
alias cuenv='printf user-alias'
if source <("$TEST_CUENV" shell init "$TEST_KIND"); then exit 1; fi
alias cuenv >/dev/null || exit 2
"#
        };
        let output = fixture.command(
            executable.to_str().ok_or("shell path is not UTF-8")?,
            script,
        )?;
        fixture.assert_success(&output);
        assert!(String::from_utf8_lossy(&output.stderr).contains("existing cuenv"));
    }
    Ok(())
}

#[test]
fn fish_rejects_trace_controls_before_applying_other_values() -> TestResult {
    let fixture = Fixture::new()?;
    let script = r#"
"$TEST_CUENV" shell init fish | source; or exit 1
set -l parts (string split ':' -- "$CUENV_MANAGED_VARS")
if __cuenv_apply "$CUENV_MANAGED_VARS" "$parts[3]" "$parts[2]" fish_trace 1 zzz_value cuenv_fish_trace_private_3e8a
    exit 2
end
set -q zzz_value; and exit 3
true
"#;
    let fish = interpreter("fish")?;
    let output = fixture.command(fish.to_str().ok_or("shell path is not UTF-8")?, script)?;
    fixture.assert_success(&output);
    Ok(())
}

#[test]
fn posix_rejects_function_local_and_transforming_bindings() -> TestResult {
    for shell in ["bash", "zsh"] {
        let fixture = Fixture::new()?;
        let executable = interpreter(shell)?;
        let script = r#"
export KEEP=ambient REMOVE=ambient
source <("$TEST_CUENV" shell init "$TEST_KIND") || exit 1
cuenv shell refresh || exit 9
function caller {
    local KEEP=local_private
    if cuenv shell refresh; then return 1; fi
    [ "$KEEP" = local_private ] || return 2
}

caller || exit 3
[ "$KEEP" = overlay ] || exit 4
if [ "$TEST_KIND" = bash ]; then declare -u CASED=original; else typeset -L CASED=original; fi
export CASED
printf '%s\n' 'package cuenv' 'name: "a"' 'env: {CASED: "changed"}' > "$TEST_PROJECT/env.cue"
if cuenv shell refresh; then exit 5; fi
[ "$KEEP" = overlay ] && [ -n "$CUENV_MANAGED_VARS" ] || exit 6
function source {
    unset -f source
    local KEEP=local_private
    cuenv shell refresh
}
if source; then exit 7; fi
[ "$KEEP" = overlay ] || exit 8
function main {
    unset -f main
    local KEEP=local_private
    cuenv shell refresh
}
if main; then exit 10; fi
[ "$KEEP" = overlay ] || exit 11
"#;
        let output = fixture.command(
            executable.to_str().ok_or("shell path is not UTF-8")?,
            script,
        )?;
        fixture.assert_success(&output);
    }
    Ok(())
}

#[test]
fn zsh_rejects_a_self_unsetting_eval_named_function() -> TestResult {
    let fixture = Fixture::new()?;
    let zsh = interpreter("zsh")?;
    let script = r#"
export KEEP=ambient REMOVE=ambient
source <("$TEST_CUENV" shell init zsh) || exit 1
cuenv shell refresh || exit 2
rest=${CUENV_MANAGED_VARS#v1:}
session_uuid=${rest%%:*}
rest=${rest#*:}
project_hash=${rest%%:*}
function '(eval)' {
    unfunction '(eval)'
    local -x KEEP=local_private
    __cuenv_apply "$CUENV_MANAGED_VARS" "$project_hash" "$session_uuid" KEEP overlay
}
if '(eval)'; then exit 3; fi
[ "$KEEP" = overlay ] || exit 4
"#;
    let output = fixture.command(zsh.to_str().ok_or("shell path is not UTF-8")?, script)?;
    fixture.assert_success(&output);
    Ok(())
}

#[test]
fn changed_hook_inputs_refresh_from_the_original_baseline() -> TestResult {
    for shell in ["bash", "zsh", "fish"] {
        let fixture = Fixture::new()?;
        write_input_hook(&fixture.project)?;
        let executable = interpreter(shell)?;
        let script = if shell == "fish" {
            r#"
set -gx OLD ambient
set -gx BASE_PATH (string join ':' -- $PATH)
"$TEST_CUENV" allow --yes; or exit 1
"$TEST_CUENV" shell init fish | source; or exit 2
cuenv env status --wait --timeout 30 --output short; or exit 3
cuenv shell refresh; or exit 4
test (string join ':' -- $PATH) = "/cuenv-one:$BASE_PATH"; and test "$OLD" = first; or exit 5
printf '2\n' > "$TEST_PROJECT/input.txt"
cuenv env load; or exit 6
cuenv env status --wait --timeout 30 --output short; or exit 7
cuenv shell refresh; or exit 8
test (string join ':' -- $PATH) = "/cuenv-two:$BASE_PATH"; and test "$OLD" = ambient; or exit 9
"#
        } else {
            r#"
export OLD=ambient BASE_PATH="$PATH"
"$TEST_CUENV" allow --yes || exit 1
source <("$TEST_CUENV" shell init "$TEST_KIND") || exit 2
cuenv shell refresh || exit 10
cuenv env status --wait --timeout 30 --output short || exit 3
cuenv shell refresh || exit 4
[ "$PATH" = "/cuenv-one:$BASE_PATH" ] && [ "$OLD" = first ] || exit 5
printf '2\n' > "$TEST_PROJECT/input.txt"
cuenv env load || exit 6
cuenv env status --wait --timeout 30 --output short || exit 7
cuenv shell refresh || exit 8
[ "$PATH" = "/cuenv-two:$BASE_PATH" ] && [ "$OLD" = ambient ] || exit 9
"#
        };
        let output = fixture.command(
            executable.to_str().ok_or("shell path is not UTF-8")?,
            script,
        )?;
        fixture.assert_success(&output);
    }
    Ok(())
}

#[test]
fn shell_helpers_do_not_call_user_printf_eval_or_source_functions() -> TestResult {
    for shell in ["bash", "zsh", "fish"] {
        let fixture = Fixture::new()?;
        let executable = interpreter(shell)?;
        let script = if shell == "fish" {
            r#"
set -gx KEEP "$TEST_BASELINE" REMOVE ambient
function printf
    builtin printf '%s' intercepted > "$TEST_ROOT/printf-called"
    builtin printf $argv
end
function source
    builtin printf '%s' intercepted > "$TEST_ROOT/source-called"
    return 1
end
"$TEST_CUENV" shell init fish | builtin source; or exit 1
cuenv shell refresh; or exit 2
test "$KEEP" = overlay; or exit 3
test -e "$TEST_ROOT/printf-called"; and exit 4
test -e "$TEST_ROOT/source-called"; and exit 5
true
"#
        } else {
            r#"
export KEEP="$TEST_BASELINE" REMOVE=ambient
function printf { builtin printf '%s' intercepted > "$TEST_ROOT/printf-called"; builtin printf "$@"; }
function eval { builtin printf '%s' intercepted > "$TEST_ROOT/eval-called"; return 1; }
builtin source <("$TEST_CUENV" shell init "$TEST_KIND") || exit 1
cuenv shell refresh || exit 2
[ "$KEEP" = overlay ] || exit 3
[ ! -e "$TEST_ROOT/printf-called" ] || exit 4
[ ! -e "$TEST_ROOT/eval-called" ] || exit 5
"#
        };
        let output = fixture.command(
            executable.to_str().ok_or("shell path is not UTF-8")?,
            script,
        )?;
        fixture.assert_success(&output);
    }
    Ok(())
}

#[test]
fn zsh_refuses_tied_values_that_cannot_be_restored_losslessly() -> TestResult {
    let fixture = Fixture::new()?;
    let zsh = interpreter("zsh")?;
    let script = r#"
source <("$TEST_CUENV" shell init zsh) || exit 1
path=("a:b" "$TEST_ROOT/bin" /bin)
if cuenv shell refresh; then exit 2; fi
[ "$#path" = 3 ] && [ "$path[1]" = 'a:b' ] || exit 3
path=()
if cuenv shell refresh; then exit 4; fi
[ "$#path" = 0 ] || exit 5
path=("$TEST_ROOT/bin" /bin)
typeset -T BDD_TIED bdd_tied
bdd_tied=('a:b' c)
export BDD_TIED
printf '%s\n' 'package cuenv' 'name: "a"' 'env: {BDD_TIED: "overlay"}' > "$TEST_PROJECT/env.cue"
if cuenv shell refresh; then exit 6; fi
[ "$#bdd_tied" = 2 ] && [ "$bdd_tied[1]" = 'a:b' ] && [ "$bdd_tied[2]" = c ] || exit 7
"#;
    let output = fixture.command(zsh.to_str().ok_or("shell path is not UTF-8")?, script)?;
    fixture.assert_success(&output);
    Ok(())
}

#[test]
fn fish_preserves_universal_updates_and_refuses_function_local_shadows() -> TestResult {
    let fixture = Fixture::new()?;
    fs::write(
        fixture.project.join("env.cue"),
        "package cuenv\nname: \"a\"\nenv: {CUENV_SCOPE_PROBE: \"overlay\"}\n",
    )?;
    let script = r#"
if set -q -g CUENV_SCOPE_PROBE; set -e -g CUENV_SCOPE_PROBE; end
set -Ux CUENV_SCOPE_PROBE original
"$TEST_CUENV" shell init fish | builtin source; or exit 1
test "$CUENV_SCOPE_PROBE" = overlay; or exit 2
"$TEST_SHELL" -c 'set -Ux CUENV_SCOPE_PROBE updated'; or exit 3
function scoped_change
    set -lx CUENV_SCOPE_PROBE local_private
    cd "$TEST_OTHER"; or return 4
    test "$CUENV_SCOPE_PROBE" = local_private; or return 5
end
scoped_change; or exit 6
test "$CUENV_SCOPE_PROBE" = overlay; and set -q -g CUENV_SCOPE_PROBE; or exit 7
cuenv shell refresh; or exit 8
test "$CUENV_SCOPE_PROBE" = updated; and not set -q -g CUENV_SCOPE_PROBE; or exit 9
set -e -U CUENV_SCOPE_PROBE
"#;
    let fish = interpreter("fish")?;
    let output = fixture.command(fish.to_str().ok_or("shell path is not UTF-8")?, script)?;
    fixture.assert_success(&output);
    Ok(())
}
