//! Executable coverage for advanced hook behavior.

use std::collections::HashMap;
use std::path::Path;
use std::process::Output;
use std::time::{Duration, Instant};

use cucumber::codegen::anyhow::{Context, anyhow, bail, ensure};
use cucumber::{given, then, when};
use cuenv_hooks::{
    ExecutionStatus, Hook, HookExecutionConfig, HookExecutionIdentity, HookExecutionState,
    HookRunState, HookRunUpdate, StateManager,
};
use tokio::fs;
use tokio::process::Command;
use uuid::Uuid;

use super::{StepResult, TestWorld};

#[derive(Debug, Default)]
pub(super) struct AdvancedEvidence {
    case: String,
    state: Option<HookExecutionState>,
    previous: Option<HookExecutionState>,
    environment: HashMap<String, String>,
    other_environment: HashMap<String, String>,
    output: String,
    status: String,
    elapsed: Duration,
    flags: HashMap<&'static str, bool>,
}

struct FixtureHook<'hook> {
    name: &'hook str,
    order: i32,
    script: &'hook str,
    source: bool,
}

fn cue_project(hooks: &[FixtureHook<'_>]) -> StepResult<String> {
    use std::fmt::Write;

    let mut cue = String::from("package cuenv\nname: \"advanced-hooks\"\nhooks: onEnter: {\n");
    for hook in hooks {
        let name = serde_json::to_string(hook.name)?;
        let script = serde_json::to_string(hook.script)?;
        writeln!(
            &mut cue,
            "{name}: {{ order: {}, command: \"sh\", args: [\"-c\", {script}], source: {} }}",
            hook.order, hook.source
        )?;
    }
    cue.push_str("}\n");
    Ok(cue)
}

fn source_script(exports: &str) -> String {
    format!("printf '%s\\n' '{}'", exports.replace('\'', "'\\''"))
}

fn fixture_project(case: &str) -> StepResult<String> {
    let source = match case {
        "multiline" => Some("export BDD_MULTILINE='first\nsecond'\nexport BDD_OTHER='intact'"),
        "unicode" => Some("export BDD_UNICODE='café 漢字'\nexport BDD_EMOJI='🧭🚀'"),
        "special" => Some(
            "export BDD_EQUALS='left=right'\nexport BDD_QUOTES='\"quoted\"'\nexport BDD_SLASH='a\\b'",
        ),
        "empty" => Some("export BDD_EMPTY=''\nunset BDD_ABSENT"),
        _ => None,
    };
    if let Some(exports) = source {
        let script = source_script(exports);
        return cue_project(&[FixtureHook {
            name: "source",
            order: 100,
            script: &script,
            source: true,
        }]);
    }
    let hooks = match case {
        "sessions" => vec![FixtureHook {
            name: "session",
            order: 100,
            script: "mkdir guard || printf overlap > overlap; printf '%s\\n' \"$BDD_SESSION\" >> runs; : > started; sleep 1; rmdir guard; printf \"export BDD_OWNER='%s'\\n\" \"$BDD_SESSION\"",
            source: true,
        }],
        "priority" => vec![
            FixtureHook {
                name: "a",
                order: 50,
                script: "printf A >> order",
                source: false,
            },
            FixtureHook {
                name: "b",
                order: 100,
                script: "printf B >> order",
                source: false,
            },
            FixtureHook {
                name: "c",
                order: 150,
                script: "printf C >> order",
                source: false,
            },
        ],
        "stable" => vec![
            FixtureHook {
                name: "z",
                order: 100,
                script: "printf z >> order",
                source: false,
            },
            FixtureHook {
                name: "a",
                order: 100,
                script: "printf a >> order",
                source: false,
            },
            FixtureHook {
                name: "m",
                order: 100,
                script: "printf m >> order",
                source: false,
            },
        ],
        "failure" => vec![FixtureHook {
            name: "failure",
            order: 100,
            script: "exit 12",
            source: false,
        }],
        "corrupt" => vec![FixtureHook {
            name: "count",
            order: 100,
            script: "printf x >> runs",
            source: false,
        }],
        "cancel" | "restart" => vec![FixtureHook {
            name: "slow",
            order: 100,
            script: "if test -f retry_ready; then printf done > retry_done; else printf '%s' \"$$\" > child_pid; exec sleep 30; fi",
            source: false,
        }],
        "timeout" | "fast" | "large" | "binary" | "directories" => Vec::new(),
        other => bail!("unknown advanced hook fixture {other}"),
    };
    cue_project(&hooks)
}

#[given(expr = "an advanced {string} hook fixture")]
async fn advanced_fixture(world: &mut TestWorld, case: String) -> StepResult {
    let project = fixture_project(&case)?;
    world.write_test_project("advanced_hook", &project).await?;
    world.advanced.case = case;
    Ok(())
}

fn session(world: &TestWorld) -> StepResult<Uuid> {
    Uuid::parse_str(&world.hook_session).context("invalid test hook session")
}

async fn cli(world: &TestWorld, args: &[&str], session: Uuid) -> StepResult<Output> {
    let mut command = Command::new(&world.cuenv_binary);
    command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", &world.state_dir)
        .env("CUENV_STATE_DIR", &world.state_dir)
        .env("CUENV_APPROVAL_FILE", world.state_file("approved.json"))
        .env("CUENV_EXECUTABLE", &world.cuenv_binary)
        .env("CUENV_HOOK_SESSION", session.to_string())
        .env("BDD_SESSION", session.to_string())
        .current_dir(&world.current_dir)
        .args(args);
    command.output().await.context("failed to run cuenv")
}

fn successful(output: &Output, operation: &str) -> StepResult {
    ensure!(
        output.status.success(),
        "{operation} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

async fn approve(world: &TestWorld, session: Uuid) -> StepResult {
    successful(&cli(world, &["allow", "--yes"], session).await?, "allow")
}

async fn start(world: &TestWorld, session: Uuid) -> StepResult {
    successful(&cli(world, &["env", "load"], session).await?, "env load")
}

async fn terminal_state(
    world: &TestWorld,
    session: Uuid,
    previous: Option<Uuid>,
) -> StepResult<HookExecutionState> {
    let directory = fs::canonicalize(&world.current_dir).await?;
    let manager = StateManager::new(world.state_dir.clone());
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Some(state) = manager
                .list_active_states()
                .await?
                .into_iter()
                .find(|state| {
                    state.directory_path == directory
                        && state.session_id == session
                        && Some(state.run_id) != previous
                        && state.is_complete()
                })
            {
                return Ok::<_, cuenv_hooks::Error>(state);
            }
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
    })
    .await
    .context("timed out waiting for the hook run")?
    .map_err(Into::into)
}

async fn captured_environment(
    world: &TestWorld,
    session: Uuid,
) -> StepResult<HashMap<String, String>> {
    let output = cli(world, &["env", "check", "--shell", "bash"], session).await?;
    successful(&output, "env check")?;
    let exports = String::from_utf8(output.stdout).context("shell output was not UTF-8")?;
    let script = format!("{exports}\ncommand env -0");
    let output = Command::new("bash")
        .arg("-c")
        .arg(script)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .output()
        .await
        .context("failed to execute Bash export")?;
    successful(&output, "Bash export")?;
    let mut environment = HashMap::new();
    for entry in output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        let entry = std::str::from_utf8(entry).context("environment was not UTF-8")?;
        let (name, value) = entry
            .split_once('=')
            .ok_or_else(|| anyhow!("invalid environment entry"))?;
        environment.insert(name.to_string(), value.to_string());
    }
    Ok(environment)
}

fn executor_hook(directory: &Path, script: &str) -> StepResult<Hook> {
    Ok(Hook {
        order: 100,
        propagate: false,
        command: "sh".to_string(),
        args: vec!["-c".to_string(), script.to_string()],
        dir: Some(TestWorld::path_arg(directory)?),
        inputs: Vec::new(),
        source: Some(false),
    })
}

async fn execute_reserved(
    world: &TestWorld,
    hooks: Vec<Hook>,
    timeout_seconds: u64,
) -> StepResult<HookExecutionState> {
    let manager = StateManager::new(world.state_dir.clone());
    let directory = fs::canonicalize(&world.current_dir).await?;
    let session_id = session(world)?;
    let config_hash = cuenv_hooks::compute_execution_hash(&hooks, &directory)?;
    let instance_hash = cuenv_hooks::compute_instance_hash(&directory, &config_hash, session_id);
    let mut state = HookExecutionState::new(
        HookExecutionIdentity {
            directory_path: directory,
            instance_hash,
            session_id,
            config_hash,
        },
        hooks.clone(),
    );
    let identity = manager.begin_run(&mut state).await?;
    ensure!(
        manager.join_run(&identity).await? == HookRunUpdate::Applied,
        "worker could not join"
    );
    let config = HookExecutionConfig {
        default_timeout_seconds: timeout_seconds,
        fail_fast: true,
        state_dir: None,
    };
    cuenv_hooks::execute_hooks(
        hooks,
        &config,
        HookRunState {
            manager: &manager,
            state: &mut state,
            identity: &identity,
        },
    )
    .await?;
    if state.status == ExecutionStatus::Completed {
        ensure!(
            manager.update_run(&identity, &state).await? == HookRunUpdate::Applied,
            "could not finish run"
        );
    }
    Ok(state)
}

async fn run_executor_case(world: &mut TestWorld) -> StepResult {
    let case = world.advanced.case.as_str();
    let (scripts, timeout_seconds): (Vec<&str>, u64) = match case {
        "timeout" => (
            vec![
                "printf '%s' \"$$\" > timeout_pid; exec sleep 30",
                "printf unexpected > next_hook",
            ],
            1,
        ),
        "fast" => (vec!["printf fast"], 5),
        "large" => (
            vec!["printf BDD_LARGE_SENTINEL; yes X | head -c 10485760"],
            30,
        ),
        "binary" => (vec!["printf '\\377\\000\\376'"], 5),
        _ => bail!("unknown executor fixture"),
    };
    let hooks = scripts
        .into_iter()
        .map(|script| executor_hook(&world.current_dir, script))
        .collect::<StepResult<Vec<_>>>()?;
    let started = Instant::now();
    let state = execute_reserved(world, hooks, timeout_seconds).await?;
    world.advanced.elapsed = started.elapsed();
    world.advanced.flags.insert(
        "next_missing",
        !world.current_dir.join("next_hook").exists(),
    );
    if case == "timeout" {
        let pid = fs::read_to_string(world.current_dir.join("timeout_pid"))
            .await?
            .parse::<u32>()?;
        world
            .advanced
            .flags
            .insert("child_stopped", child_has_stopped(pid)?);
    }
    if case == "large" {
        let mut clean = true;
        let mut entries = fs::read_dir(&world.state_dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            if entry.file_type().await?.is_file() {
                let bytes = fs::read(entry.path()).await?;
                clean &= !bytes
                    .windows(b"BDD_LARGE_SENTINEL".len())
                    .any(|window| window == b"BDD_LARGE_SENTINEL");
            }
        }
        world.advanced.flags.insert("state_clean", clean);
    }
    world.advanced.state = Some(state);
    Ok(())
}

async fn run_source_case(world: &mut TestWorld) -> StepResult {
    let id = session(world)?;
    approve(world, id).await?;
    start(world, id).await?;
    let state = terminal_state(world, id, None).await?;
    ensure!(
        state.status == ExecutionStatus::Completed,
        "source hook failed: {:?}",
        state.status
    );
    world.advanced.environment = captured_environment(world, id).await?;
    world.advanced.state = Some(state);
    Ok(())
}

async fn run_order_case(world: &mut TestWorld) -> StepResult {
    let id = session(world)?;
    approve(world, id).await?;
    start(world, id).await?;
    let first = terminal_state(world, id, None).await?;
    ensure!(
        first.status == ExecutionStatus::Completed,
        "first hook run failed"
    );
    if world.advanced.case == "stable" {
        let second_session = Uuid::new_v4();
        start(world, second_session).await?;
        let second = terminal_state(world, second_session, None).await?;
        ensure!(
            second.status == ExecutionStatus::Completed,
            "second hook run failed"
        );
        world.advanced.previous = Some(first);
        world.advanced.state = Some(second);
    } else {
        world.advanced.state = Some(first);
    }
    world.advanced.output = fs::read_to_string(world.current_dir.join("order")).await?;
    Ok(())
}

async fn run_failure_case(world: &mut TestWorld) -> StepResult {
    let id = session(world)?;
    approve(world, id).await?;
    start(world, id).await?;
    let failed = terminal_state(world, id, None).await?;
    let status = cli(world, &["env", "status", "--wait", "--timeout", "5"], id).await?;
    successful(&status, "env status")?;
    world.advanced.status = String::from_utf8_lossy(&status.stdout).into_owned();
    let fixed = cue_project(&[FixtureHook {
        name: "failure",
        order: 100,
        script: "printf fixed > fixed",
        source: false,
    }])?;
    world.write_current_env_cue(&fixed).await?;
    approve(world, id).await?;
    start(world, id).await?;
    let completed = terminal_state(world, id, Some(failed.run_id)).await?;
    world
        .advanced
        .flags
        .insert("fixed_marker", world.current_dir.join("fixed").exists());
    world.advanced.previous = Some(failed);
    world.advanced.state = Some(completed);
    Ok(())
}

async fn run_corrupt_case(world: &mut TestWorld) -> StepResult {
    let id = session(world)?;
    approve(world, id).await?;
    start(world, id).await?;
    let completed = terminal_state(world, id, None).await?;
    ensure!(
        completed.status == ExecutionStatus::Completed,
        "hook did not complete"
    );
    let manager = StateManager::new(world.state_dir.clone());
    let path = manager.try_state_file_path(&completed.instance_hash)?;
    fs::write(&path, b"{corrupted state").await?;
    let output = cli(world, &["export", "--shell", "bash"], id).await?;
    world
        .advanced
        .flags
        .insert("rejected", !output.status.success());
    world
        .advanced
        .flags
        .insert("retained", fs::read(&path).await? == b"{corrupted state");
    world.advanced.output = fs::read_to_string(world.current_dir.join("runs")).await?;
    Ok(())
}

async fn wait_for_child(world: &TestWorld) -> StepResult<cuenv_hooks::HookReservation> {
    let manager = StateManager::new(world.state_dir.clone());
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let Some(reservation) = manager.current_reservation(&world.current_dir).await?
                && reservation.child_pid.is_some()
                && world.current_dir.join("child_pid").exists()
            {
                return Ok::<_, cuenv_hooks::Error>(reservation);
            }
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
    })
    .await
    .context("timed out waiting for a hook child")?
    .map_err(Into::into)
}

#[cfg(unix)]
fn child_has_stopped(raw_pid: u32) -> StepResult<bool> {
    let raw_pid = i32::try_from(raw_pid).context("child PID does not fit the platform")?;
    let pid =
        rustix::process::Pid::from_raw(raw_pid).ok_or_else(|| anyhow!("invalid child PID"))?;
    Ok(matches!(
        rustix::process::test_kill_process(pid),
        Err(rustix::io::Errno::SRCH)
    ))
}

#[cfg(not(unix))]
fn child_has_stopped(_raw_pid: u32) -> StepResult<bool> {
    bail!("child liveness verification requires a Unix process API")
}

async fn run_cancel_case(world: &mut TestWorld) -> StepResult {
    let id = session(world)?;
    approve(world, id).await?;
    start(world, id).await?;
    let reservation = wait_for_child(world).await?;
    let pid = reservation
        .child_pid
        .ok_or_else(|| anyhow!("missing hook child PID"))?;
    let manager = StateManager::new(world.state_dir.clone());
    ensure!(
        manager
            .request_cancellation(&reservation.identity, Some("BDD cancellation".to_string()))
            .await?
            == HookRunUpdate::CancelRequested,
        "cancellation was not requested"
    );
    let cancelled = terminal_state(world, id, None).await?;
    world
        .advanced
        .flags
        .insert("child_stopped", child_has_stopped(pid)?);
    world.advanced.flags.insert(
        "released",
        manager
            .current_reservation(&world.current_dir)
            .await?
            .is_none(),
    );
    if world.advanced.case == "restart" {
        fs::write(world.current_dir.join("retry_ready"), b"ready").await?;
        start(world, id).await?;
        let completed = terminal_state(world, id, Some(cancelled.run_id)).await?;
        world
            .advanced
            .flags
            .insert("retry_done", world.current_dir.join("retry_done").exists());
        world.advanced.previous = Some(cancelled);
        world.advanced.state = Some(completed);
    } else {
        world.advanced.state = Some(cancelled);
    }
    Ok(())
}

async fn run_sessions_case(world: &mut TestWorld) -> StepResult {
    let first_id = session(world)?;
    let second_id = Uuid::new_v4();
    approve(world, first_id).await?;
    start(world, first_id).await?;
    let manager = StateManager::new(world.state_dir.clone());
    let reservation = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if world.current_dir.join("started").exists()
                && let Some(reservation) = manager.current_reservation(&world.current_dir).await?
            {
                return Ok::<_, cuenv_hooks::Error>(reservation);
            }
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
    })
    .await
    .context("timed out waiting for the first session")??;
    let active = manager
        .load_state(&reservation.identity.instance_hash)
        .await?
        .ok_or_else(|| anyhow!("missing active session state"))?;
    let mut competing = HookExecutionState::new(
        HookExecutionIdentity {
            directory_path: world.current_dir.clone(),
            instance_hash: String::new(),
            session_id: second_id,
            config_hash: active.config_hash,
        },
        Vec::new(),
    );
    let blocked = matches!(
        manager.begin_run(&mut competing).await,
        Err(cuenv_hooks::Error::Busy)
    );
    let first = terminal_state(world, first_id, None).await?;
    ensure!(
        first.status == ExecutionStatus::Completed,
        "first session failed"
    );
    start(world, second_id).await?;
    let second = terminal_state(world, second_id, None).await?;
    world.advanced.environment = captured_environment(world, first_id).await?;
    world.advanced.other_environment = captured_environment(world, second_id).await?;
    world.advanced.output = fs::read_to_string(world.current_dir.join("runs")).await?;
    world.advanced.flags.insert("blocked", blocked);
    world
        .advanced
        .flags
        .insert("no_overlap", !world.current_dir.join("overlap").exists());
    world.advanced.previous = Some(first);
    world.advanced.state = Some(second);
    Ok(())
}

async fn run_directories_case(world: &mut TestWorld) -> StepResult {
    let root = world.current_dir.clone();
    let id = session(world)?;
    for label in ["first", "second"] {
        let directory = root.join(label);
        fs::create_dir_all(&directory).await?;
        let script = source_script(&format!("export BDD_DIRECTORY='{label}'"));
        let cue = cue_project(&[FixtureHook {
            name: "directory",
            order: 100,
            script: &script,
            source: true,
        }])?;
        fs::write(directory.join("env.cue"), cue).await?;
        world.current_dir = directory;
        approve(world, id).await?;
    }
    for index in 0..10 {
        world.current_dir = root.join(if index % 2 == 0 { "first" } else { "second" });
        successful(
            &cli(world, &["export", "--shell", "bash"], id).await?,
            "directory export",
        )?;
    }
    world.current_dir = root.join("first");
    let first = terminal_state(world, id, None).await?;
    world.current_dir = root.join("second");
    let second = terminal_state(world, id, None).await?;
    world.advanced.environment = captured_environment(world, id).await?;
    world.advanced.previous = Some(first);
    world.advanced.state = Some(second);
    Ok(())
}

#[when(expr = "I run the advanced hook fixture")]
async fn run_advanced_fixture(world: &mut TestWorld) -> StepResult {
    match world.advanced.case.as_str() {
        "timeout" | "fast" | "large" | "binary" => run_executor_case(world).await,
        "multiline" | "unicode" | "special" | "empty" => run_source_case(world).await,
        "priority" | "stable" => run_order_case(world).await,
        "failure" => run_failure_case(world).await,
        "corrupt" => run_corrupt_case(world).await,
        "cancel" | "restart" => run_cancel_case(world).await,
        "sessions" => run_sessions_case(world).await,
        "directories" => run_directories_case(world).await,
        other => bail!("unknown advanced fixture {other}"),
    }
}

fn state(world: &TestWorld) -> StepResult<&HookExecutionState> {
    world
        .advanced
        .state
        .as_ref()
        .ok_or_else(|| anyhow!("missing hook result"))
}

fn previous(world: &TestWorld) -> StepResult<&HookExecutionState> {
    world
        .advanced
        .previous
        .as_ref()
        .ok_or_else(|| anyhow!("missing earlier hook result"))
}

fn flag(world: &TestWorld, name: &'static str) -> StepResult {
    ensure!(
        world.advanced.flags.get(name) == Some(&true),
        "expected {name}"
    );
    Ok(())
}

#[then(expr = "the direct child times out and the next hook does not run")]
fn timeout_stops_run(world: &mut TestWorld) -> StepResult {
    let state = state(world)?;
    ensure!(
        state.status == ExecutionStatus::Failed && state.completed_hooks == 1,
        "hook did not fail fast"
    );
    let error = state
        .hook_results
        .get(&0)
        .and_then(|result| result.error.as_deref())
        .unwrap_or_default();
    ensure!(
        error.contains("timed out"),
        "missing timeout result: {error}"
    );
    ensure!(
        world.advanced.elapsed < Duration::from_secs(15),
        "timeout did not stop promptly"
    );
    flag(world, "next_missing")?;
    flag(world, "child_stopped")
}

#[then(expr = "the fast hook completes within its deadline")]
fn fast_hook_completes(world: &mut TestWorld) -> StepResult {
    let state = state(world)?;
    ensure!(
        state.status == ExecutionStatus::Completed,
        "fast hook failed"
    );
    ensure!(
        state
            .hook_results
            .get(&0)
            .is_some_and(|result| result.success && result.stdout == "fast")
    );
    ensure!(
        world.advanced.elapsed < Duration::from_secs(5),
        "fast hook exceeded its deadline"
    );
    Ok(())
}

#[then(expr = "the sessions have separate results and no overlapping hooks")]
fn sessions_are_isolated(world: &mut TestWorld) -> StepResult {
    let first = previous(world)?;
    let second = state(world)?;
    ensure!(
        first.status == ExecutionStatus::Completed && second.status == ExecutionStatus::Completed
    );
    ensure!(first.session_id != second.session_id && first.run_id != second.run_id);
    ensure!(world.advanced.environment.get("BDD_OWNER") == Some(&first.session_id.to_string()));
    ensure!(
        world.advanced.other_environment.get("BDD_OWNER") == Some(&second.session_id.to_string())
    );
    ensure!(world.advanced.output == format!("{}\n{}\n", first.session_id, second.session_id));
    flag(world, "blocked")?;
    flag(world, "no_overlap")
}

#[then(expr = "both directories complete and the final directory supplies its environment")]
fn directories_remain_separate(world: &mut TestWorld) -> StepResult {
    let first = previous(world)?;
    let second = state(world)?;
    ensure!(
        first.status == ExecutionStatus::Completed && second.status == ExecutionStatus::Completed
    );
    ensure!(first.directory_path != second.directory_path);
    ensure!(
        world
            .advanced
            .environment
            .get("BDD_DIRECTORY")
            .map(String::as_str)
            == Some("second")
    );
    Ok(())
}

#[then(expr = "the multiline value and its neighboring variable are intact")]
fn multiline_is_preserved(world: &mut TestWorld) -> StepResult {
    ensure!(
        world
            .advanced
            .environment
            .get("BDD_MULTILINE")
            .map(String::as_str)
            == Some("first\nsecond")
    );
    ensure!(
        world
            .advanced
            .environment
            .get("BDD_OTHER")
            .map(String::as_str)
            == Some("intact")
    );
    Ok(())
}

#[then(expr = "Unicode and emoji values are intact")]
fn unicode_is_preserved(world: &mut TestWorld) -> StepResult {
    ensure!(
        world
            .advanced
            .environment
            .get("BDD_UNICODE")
            .map(String::as_str)
            == Some("café 漢字")
    );
    ensure!(
        world
            .advanced
            .environment
            .get("BDD_EMOJI")
            .map(String::as_str)
            == Some("🧭🚀")
    );
    Ok(())
}

#[then(expr = "equals signs, quotes, and backslashes are intact")]
fn special_characters_are_preserved(world: &mut TestWorld) -> StepResult {
    ensure!(
        world
            .advanced
            .environment
            .get("BDD_EQUALS")
            .map(String::as_str)
            == Some("left=right")
    );
    ensure!(
        world
            .advanced
            .environment
            .get("BDD_QUOTES")
            .map(String::as_str)
            == Some("\"quoted\"")
    );
    ensure!(
        world
            .advanced
            .environment
            .get("BDD_SLASH")
            .map(String::as_str)
            == Some("a\\b")
    );
    Ok(())
}

#[then(expr = "the empty variable exists and the absent variable does not")]
fn empty_is_distinct_from_unset(world: &mut TestWorld) -> StepResult {
    ensure!(
        world
            .advanced
            .environment
            .get("BDD_EMPTY")
            .map(String::as_str)
            == Some("")
    );
    ensure!(!world.advanced.environment.contains_key("BDD_ABSENT"));
    Ok(())
}

#[then(expr = "hooks execute in priority order")]
fn priority_order(world: &mut TestWorld) -> StepResult {
    ensure!(state(world)?.status == ExecutionStatus::Completed);
    ensure!(
        world.advanced.output == "ABC",
        "unexpected priority order: {}",
        world.advanced.output
    );
    Ok(())
}

#[then(expr = "both executions have the same name order")]
fn stable_order(world: &mut TestWorld) -> StepResult {
    ensure!(state(world)?.run_id != previous(world)?.run_id);
    ensure!(state(world)?.session_id != previous(world)?.session_id);
    ensure!(
        world.advanced.output == "amzamz",
        "unexpected stable order: {}",
        world.advanced.output
    );
    Ok(())
}

#[then(expr = "status reports failure and the corrected hook succeeds")]
fn failure_is_recoverable(world: &mut TestWorld) -> StepResult {
    ensure!(previous(world)?.status == ExecutionStatus::Failed);
    ensure!(
        world.advanced.status.to_ascii_lowercase().contains("fail"),
        "status did not report failure"
    );
    ensure!(state(world)?.status == ExecutionStatus::Completed);
    flag(world, "fixed_marker")
}

#[then(expr = "corruption is reported without deleting the file or rerunning hooks")]
fn corrupt_state_fails_closed(world: &mut TestWorld) -> StepResult {
    flag(world, "rejected")?;
    flag(world, "retained")?;
    ensure!(
        world.advanced.output == "x",
        "corrupt state triggered another hook"
    );
    Ok(())
}

#[then(expr = "cancellation is acknowledged after the child stops")]
fn cancellation_reaps_child(world: &mut TestWorld) -> StepResult {
    ensure!(state(world)?.status == ExecutionStatus::Cancelled);
    flag(world, "child_stopped")?;
    flag(world, "released")
}

#[then(expr = "a fresh run completes after cancellation")]
fn cancelled_run_restarts(world: &mut TestWorld) -> StepResult {
    ensure!(previous(world)?.status == ExecutionStatus::Cancelled);
    ensure!(state(world)?.status == ExecutionStatus::Completed);
    ensure!(state(world)?.run_id != previous(world)?.run_id);
    flag(world, "child_stopped")?;
    flag(world, "released")?;
    flag(world, "retry_done")
}

#[then(expr = "the complete large output is available without being persisted")]
fn large_output_is_memory_only(world: &mut TestWorld) -> StepResult {
    let state = state(world)?;
    ensure!(state.status == ExecutionStatus::Completed);
    let output = &state
        .hook_results
        .get(&0)
        .ok_or_else(|| anyhow!("missing hook output"))?
        .stdout;
    ensure!(output.starts_with("BDD_LARGE_SENTINEL"));
    ensure!(output.len() == "BDD_LARGE_SENTINEL".len() + 10_485_760);
    flag(world, "state_clean")
}

#[then(expr = "invalid UTF-8 is replaced and the hook succeeds")]
fn binary_output_is_safe(world: &mut TestWorld) -> StepResult {
    let state = state(world)?;
    ensure!(state.status == ExecutionStatus::Completed);
    let output = &state
        .hook_results
        .get(&0)
        .ok_or_else(|| anyhow!("missing hook output"))?
        .stdout;
    ensure!(output == "\u{fffd}\0\u{fffd}", "unexpected binary output");
    Ok(())
}
