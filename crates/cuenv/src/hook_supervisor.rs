use cuenv::cli::CliError;
use cuenv_hooks::{
    ExecutionStatus, Hook, HookExecutionConfig, HookExecutionState, StateManager, execute_hooks,
};
use std::path::PathBuf;

struct HookSupervisorArgs {
    directory_path: PathBuf,
    instance_hash: String,
    hooks_file: PathBuf,
    config_file: PathBuf,
}

struct HookSupervisorFiles {
    hooks: Vec<Hook>,
    config: HookExecutionConfig,
}

struct HookSupervisorState {
    manager: StateManager,
    pid_file: PathBuf,
}

struct HookSupervisorRun<'a> {
    files: HookSupervisorFiles,
    args: &'a HookSupervisorArgs,
    state_context: &'a HookSupervisorState,
    state: &'a mut HookExecutionState,
}

/// Run as a hook supervisor process.
pub async fn run_hook_supervisor(args: Vec<String>) -> Result<(), CliError> {
    let supervisor_args = parse_hook_supervisor_args(&args);

    init_hook_supervisor_logging();
    enter_hook_supervisor_directory(&supervisor_args.directory_path)?;
    log_hook_supervisor_start(&supervisor_args, &args);

    let files = load_hook_supervisor_files(&supervisor_args)?;
    let state_context = prepare_hook_supervisor_state(&supervisor_args, &files.config)?;
    let mut state =
        load_hook_supervisor_state(&state_context, &supervisor_args.instance_hash).await?;

    execute_hook_supervisor_run(HookSupervisorRun {
        files,
        args: &supervisor_args,
        state_context: &state_context,
        state: &mut state,
    })
    .await?;

    std::fs::remove_file(&state_context.pid_file).ok();
    cuenv_events::emit_supervisor_log!("supervisor", "Completed successfully");
    Ok(())
}

fn parse_hook_supervisor_args(args: &[String]) -> HookSupervisorArgs {
    let mut parsed = HookSupervisorArgs {
        directory_path: PathBuf::new(),
        instance_hash: String::new(),
        hooks_file: PathBuf::new(),
        config_file: PathBuf::new(),
    };

    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--directory" => {
                parsed.directory_path = PathBuf::from(&args[i + 1]);
                i += 2;
            }
            "--instance-hash" => {
                parsed.instance_hash.clone_from(&args[i + 1]);
                i += 2;
            }
            "--config-hash" => {
                i += 2;
            }
            "--hooks-file" => {
                parsed.hooks_file = PathBuf::from(&args[i + 1]);
                i += 2;
            }
            "--config-file" => {
                parsed.config_file = PathBuf::from(&args[i + 1]);
                i += 2;
            }
            _ => i += 1,
        }
    }

    parsed
}

fn init_hook_supervisor_logging() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .try_init();
}

fn enter_hook_supervisor_directory(directory_path: &PathBuf) -> Result<(), CliError> {
    if let Err(e) = std::env::set_current_dir(directory_path) {
        cuenv_events::emit_supervisor_log!(
            "supervisor",
            format!(
                "Failed to change directory to {}: {}",
                directory_path.display(),
                e
            )
        );
        return Err(CliError::other(format!("Failed to change directory: {e}")));
    }

    Ok(())
}

fn log_hook_supervisor_start(supervisor_args: &HookSupervisorArgs, raw_args: &[String]) {
    cuenv_events::emit_supervisor_log!("supervisor", format!("Starting with args: {raw_args:?}"));
    cuenv_events::emit_supervisor_log!(
        "supervisor",
        format!("Directory: {}", supervisor_args.directory_path.display())
    );
    cuenv_events::emit_supervisor_log!(
        "supervisor",
        format!("Instance hash: {}", supervisor_args.instance_hash)
    );
    cuenv_events::emit_supervisor_log!(
        "supervisor",
        format!("Hooks file: {}", supervisor_args.hooks_file.display())
    );
    cuenv_events::emit_supervisor_log!(
        "supervisor",
        format!("Config file: {}", supervisor_args.config_file.display())
    );
}

fn load_hook_supervisor_files(
    supervisor_args: &HookSupervisorArgs,
) -> Result<HookSupervisorFiles, CliError> {
    let hooks_json = std::fs::read_to_string(&supervisor_args.hooks_file)
        .map_err(|e| CliError::other(format!("Failed to read hooks file: {e}")))?;
    let config_json = std::fs::read_to_string(&supervisor_args.config_file)
        .map_err(|e| CliError::other(format!("Failed to read config file: {e}")))?;

    let hooks = serde_json::from_str(&hooks_json)
        .map_err(|e| CliError::other(format!("Failed to deserialize hooks: {e}")))?;
    let config = serde_json::from_str(&config_json)
        .map_err(|e| CliError::other(format!("Failed to deserialize config: {e}")))?;

    std::fs::remove_file(&supervisor_args.hooks_file).ok();
    std::fs::remove_file(&supervisor_args.config_file).ok();

    Ok(HookSupervisorFiles { hooks, config })
}

fn prepare_hook_supervisor_state(
    supervisor_args: &HookSupervisorArgs,
    config: &HookExecutionConfig,
) -> Result<HookSupervisorState, CliError> {
    let state_dir = match config.state_dir.clone() {
        Some(dir) => dir,
        None => StateManager::default_state_dir()
            .map_err(|e| CliError::other(format!("failed to get default state dir: {e}")))?,
    };
    cuenv_events::emit_supervisor_log!(
        "supervisor",
        format!("Using state dir: {}", state_dir.display())
    );

    let manager = StateManager::new(state_dir);
    let state_file = manager.get_state_file_path(&supervisor_args.instance_hash);
    cuenv_events::emit_supervisor_log!(
        "supervisor",
        format!("Looking for state file: {}", state_file.display())
    );

    let pid_file = state_file.with_extension("pid");
    std::fs::write(&pid_file, format!("{}", std::process::id()))
        .map_err(|e| CliError::other(format!("Failed to write PID file: {e}")))?;

    Ok(HookSupervisorState { manager, pid_file })
}

async fn load_hook_supervisor_state(
    state_context: &HookSupervisorState,
    instance_hash: &str,
) -> Result<HookExecutionState, CliError> {
    state_context
        .manager
        .load_state(instance_hash)
        .await
        .map_err(|e| CliError::other(format!("Failed to load state: {e}")))?
        .ok_or_else(|| CliError::other("State not found for supervisor"))
}

async fn execute_hook_supervisor_run(run: HookSupervisorRun<'_>) -> Result<(), CliError> {
    cuenv_events::emit_supervisor_log!(
        "supervisor",
        format!(
            "Executing {} hooks for directory: {}",
            run.files.hooks.len(),
            run.args.directory_path.display()
        )
    );

    let result = execute_hooks(
        run.files.hooks,
        &run.args.directory_path,
        &run.files.config,
        &run.state_context.manager,
        run.state,
    )
    .await;

    if let Err(e) = result {
        let error = e.to_string();
        save_hook_supervisor_failure(run.state_context, run.state, &error).await?;
        return Err(CliError::other(format!("Hook execution failed: {error}")));
    }

    save_hook_supervisor_success(run.state_context, run.state).await
}

async fn save_hook_supervisor_failure(
    state_context: &HookSupervisorState,
    state: &mut HookExecutionState,
    error: &str,
) -> Result<(), CliError> {
    cuenv_events::emit_supervisor_log!("supervisor", format!("Hook execution failed: {error}"));
    state.status = ExecutionStatus::Failed;
    state.error_message = Some(error.to_string());
    state.finished_at = Some(chrono::Utc::now());
    state_context
        .manager
        .save_state(state)
        .await
        .map_err(|e| CliError::other(format!("Failed to save error state: {e}")))
}

async fn save_hook_supervisor_success(
    state_context: &HookSupervisorState,
    state: &HookExecutionState,
) -> Result<(), CliError> {
    cuenv_events::emit_supervisor_log!(
        "supervisor",
        format!(
            "Saving final state with {} environment variables",
            state.environment_vars.len()
        )
    );
    state_context
        .manager
        .save_state(state)
        .await
        .map_err(|e| CliError::other(format!("Failed to save final state: {e}")))
}
