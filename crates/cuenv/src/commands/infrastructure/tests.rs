use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use cuenv_infrastructure::{
    ConditionalPut, InfrastructureError, LockInformation, LockRequest, ManagedResource,
    MemoryStateStore, OwnerClaim, OwnerClaimMode, ProjectInstance, RecordVersion, RecoverOverwrite,
    ResourceAddress, StateLock, StateStore, TenantKey, TenantOwner, UnrecordedStore,
};
use cuenv_manifest::environment::EnvValue;
use tokio::sync::mpsc;

use super::evaluation::{self, NameCheck, TargetRequest};
use super::interrupts::{Interrupts, SignalSource};
use super::output::{Finish, Output};
use super::{
    AnswerFuture, Answers, CommandContext, ConfirmationPolicy, EngineInputs, InfrastructureAction,
    InfrastructureOptions, StateAction, confirm, dispatch, environment_variables_for_action,
    release, run, under_lock,
};
use crate::cli::{
    CliError, EXIT_CANCELLED, EXIT_CLI, EXIT_EVAL, EXIT_INFRASTRUCTURE, EXIT_INTERRUPTED,
    EXIT_LOCKED, LockStatus, OutputFormat, error_code_for, exit_code_for,
};

const MODULE: &str = "module: \"example.com/infrastructure\"\nlanguage: version: \"v0.14.1\"\n";

const INFRASTRUCTURE: &str = "infrastructure: {\n\
     \tstate: turso: url: \"http://127.0.0.1:8080\"\n\
     \tproviders: random: {source: \"hashicorp/random\", version: \"3.7.2\"}\n\
     \tresources: pet: {type: \"random_pet\", configuration: length: 2}\n\
     }\n";

#[test]
fn state_only_actions_resolve_only_the_backend_token() {
    let environment = HashMap::from([
        (
            "TURSO_AUTH_TOKEN".to_string(),
            EnvValue::String("backend-token".to_string()),
        ),
        (
            "AWS_PROFILE".to_string(),
            EnvValue::String("platform-dev".to_string()),
        ),
    ]);
    let state_environment = environment_variables_for_action(
        &InfrastructureAction::State(StateAction::List),
        &environment,
        "TURSO_AUTH_TOKEN",
    );
    assert_eq!(
        state_environment,
        HashMap::from([(
            "TURSO_AUTH_TOKEN".to_string(),
            EnvValue::String("backend-token".to_string()),
        )])
    );

    let provider_environment = environment_variables_for_action(
        &InfrastructureAction::Plan,
        &environment,
        "TURSO_AUTH_TOKEN",
    );
    assert_eq!(provider_environment, environment);
}

fn write(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

fn module_directory() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("cuenv-infrastructure-")
        .tempdir()
        .unwrap()
}

/// Write the module and the target project `app` (package `cuenv`) into
/// `root`, at `project` relative to it.
fn write_target(root: &Path, project: &str) {
    write(root, "cue.mod/module.cue", MODULE);
    write(
        root,
        &format!("{project}/env.cue"),
        &format!("package cuenv\n\nname: \"app\"\n{INFRASTRUCTURE}"),
    );
}

/// A module with the target project `app` in `app/` (package `cuenv`).
fn module_with_target() -> tempfile::TempDir {
    let directory = module_directory();
    write_target(directory.path(), "app");
    directory
}

fn evaluate_at(project: &Path, name_check: NameCheck) -> Result<evaluation::Target, CliError> {
    evaluate_at_environment(project, name_check, None)
}

fn evaluate_at_environment(
    project: &Path,
    name_check: NameCheck,
    environment: Option<&str>,
) -> Result<evaluation::Target, CliError> {
    evaluation::evaluate(TargetRequest {
        path: project.to_str().unwrap(),
        package: "cuenv",
        name_check,
        environment,
    })
}

fn evaluate(root: &Path, name_check: NameCheck) -> Result<evaluation::Target, CliError> {
    evaluate_at(&root.join("app"), name_check)
}

#[test]
fn a_unique_project_evaluates() {
    let module = module_with_target();
    write(
        module.path(),
        "other/env.cue",
        "package cuenv\n\nname: \"other\"\n",
    );
    let target = evaluate(module.path(), NameCheck::WholeModule).unwrap();
    assert_eq!(
        target.tenant,
        TenantKey::new("example.com/infrastructure", "app").unwrap()
    );
    assert_eq!(target.instance.as_str(), "app:cuenv");
    assert_eq!(target.infrastructure.resources.len(), 1);
    assert_eq!(target.environment, None);
}

#[test]
fn named_environment_selects_only_its_complete_configuration_and_overlay() {
    let module = module_directory();
    write(module.path(), "cue.mod/module.cue", MODULE);
    write(
        module.path(),
        "app/env.cue",
        r#"package cuenv
name: "app"
env: {
  BASE: "base"
  environment: {
    dev: {BASE: "development", ONLY: "dev"}
    prod: {UNUSED: _}
  }
}

infrastructure: {
  state: turso: url: "http://127.0.0.1:8080"
  providers: legacy: {source: "hashicorp/legacy", version: "1.0.0"}
  resources: old: {type: "legacy_old"}
  environments: {
    dev: {
      providers: random: {source: "hashicorp/random", version: "3.7.2"}
      resources: pet: {type: "random_pet", configuration: length: 2}
    }
    prod: {
      providers: random: {source: "hashicorp/random", version: _}
    }
  }
}
"#,
    );
    let target = evaluate_at_environment(
        &module.path().join("app"),
        NameCheck::TargetOnly,
        Some("dev"),
    )
    .unwrap();
    assert_eq!(target.environment.as_deref(), Some("dev"));
    assert_eq!(target.infrastructure.providers.len(), 1);
    assert!(target.infrastructure.providers.contains_key("random"));
    assert!(target.infrastructure.resources.contains_key("pet"));
    assert!(!target.infrastructure.resources.contains_key("old"));
    assert!(target.infrastructure.environments.is_empty());
    assert_eq!(
        target.env.as_ref().unwrap().for_environment("dev")["BASE"].to_string_value(),
        "development"
    );
    assert!(
        !target
            .env
            .as_ref()
            .unwrap()
            .environment
            .as_ref()
            .unwrap()
            .contains_key("prod")
    );
    assert_eq!(
        target.tenant,
        TenantKey::with_environment("example.com/infrastructure", "app", "dev").unwrap()
    );
}

#[tokio::test]
async fn provider_configuration_preflight_runs_before_secret_resolution_and_state_access() {
    let module = module_directory();
    write(module.path(), "cue.mod/module.cue", MODULE);
    write(
        module.path(),
        "app/env.cue",
        r#"package cuenv
name: "app"
env: {
  SECRET_TOKEN: {resolver: "exec", command: "cuenv-secret-resolver-must-not-run"}
}
infrastructure: {
  state: turso: url: "http://127.0.0.1:1"
  providers: random: {source: "hashicorp/random", version: "~> 3.7"}
  resources: pet: {type: "random_pet", configuration: length: 2}
}
"#,
    );

    let harness = Harness::new(OutputFormat::Text, Script::Line(""));
    let options = InfrastructureOptions {
        path: module.path().join("app").to_string_lossy().into_owned(),
        package: "cuenv".into(),
        environment: None,
        action: InfrastructureAction::Plan,
        output: OutputFormat::Text,
    };
    let error = run(&options, &harness.output, &harness.interrupts)
        .await
        .unwrap_err();

    assert!(
        error.to_string().contains("invalid provider version"),
        "expected provider configuration error before secret resolution or backend access, got: {error}"
    );
}

#[test]
fn named_environment_match_is_case_sensitive_and_missing_name_fails_closed() {
    let module = module_directory();
    write(module.path(), "cue.mod/module.cue", MODULE);
    write(
        module.path(),
        "app/env.cue",
        r#"package cuenv
name: "app"
infrastructure: {
  state: turso: url: "http://127.0.0.1:8080"
  environments: Dev: {
    providers: random: {source: "hashicorp/random", version: "3.7.2"}
  }
}
"#,
    );
    let project = module.path().join("app");
    assert!(evaluate_at_environment(&project, NameCheck::TargetOnly, Some("Dev")).is_ok());
    let error = evaluate_at_environment(&project, NameCheck::TargetOnly, Some("dev")).unwrap_err();
    assert_eq!(exit_code_for(&error), EXIT_CLI);
    assert!(
        error
            .to_string()
            .contains("no infrastructure environment named 'dev'"),
        "{error}"
    );
}

#[test]
fn no_selector_keeps_legacy_configuration_without_deserializing_named_entries() {
    let module = module_directory();
    write(module.path(), "cue.mod/module.cue", MODULE);
    write(
        module.path(),
        "app/env.cue",
        r#"package cuenv
name: "app"
infrastructure: {
  state: turso: url: "http://127.0.0.1:8080"
  providers: random: {source: "hashicorp/random", version: "3.7.2"}
  resources: pet: {type: "random_pet", configuration: length: 2}
  environments: prod: {
    providers: random: {source: "hashicorp/random", version: _}
  }
}
"#,
    );
    let target = evaluate(module.path(), NameCheck::TargetOnly).unwrap();
    assert_eq!(target.environment, None);
    assert!(target.infrastructure.providers.contains_key("random"));
    assert!(target.infrastructure.resources.contains_key("pet"));
    assert!(target.infrastructure.environments.is_empty());
    assert_eq!(
        target.tenant,
        TenantKey::new("example.com/infrastructure", "app").unwrap()
    );
}

#[test]
fn selected_named_environment_must_be_concrete() {
    let module = module_directory();
    write(module.path(), "cue.mod/module.cue", MODULE);
    write(
        module.path(),
        "app/env.cue",
        r#"package cuenv
name: "app"
infrastructure: {
  state: turso: url: "http://127.0.0.1:8080"
  environments: dev: {
    providers: random: {source: "hashicorp/random", version: _}
  }
}
"#,
    );
    let error = evaluate_at_environment(
        &module.path().join("app"),
        NameCheck::TargetOnly,
        Some("dev"),
    )
    .unwrap_err();
    assert_eq!(exit_code_for(&error), EXIT_EVAL);
}

#[test]
fn selected_projection_rejects_unknown_infrastructure_fields() {
    let module = module_directory();
    write(module.path(), "cue.mod/module.cue", MODULE);
    write(
        module.path(),
        "app/env.cue",
        r#"package cuenv
name: "app"
infrastructure: {
  state: turso: url: "http://127.0.0.1:8080"
  resource: typo: {type: "random_pet"}
  environments: {
    dev: {providers: random: {source: "hashicorp/random", version: "3.7.2"}}
    prod: {providers: random: {source: "hashicorp/random", version: _}}
  }
}
"#,
    );
    let error = evaluate_at_environment(
        &module.path().join("app"),
        NameCheck::TargetOnly,
        Some("dev"),
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("unknown field `resource`"),
        "{error}"
    );
    let error = evaluate(module.path(), NameCheck::TargetOnly).unwrap_err();
    assert!(
        error.to_string().contains("unknown field `resource`"),
        "{error}"
    );
}

#[test]
fn a_duplicate_name_in_another_package_is_refused() {
    let module = module_with_target();
    write(
        module.path(),
        "elsewhere/project.cue",
        &format!("package deployment\n\nname: \"app\"\n{INFRASTRUCTURE}"),
    );
    let error = evaluate(module.path(), NameCheck::WholeModule).unwrap_err();
    assert_eq!(exit_code_for(&error), EXIT_CLI);
    assert!(
        error.to_string().contains("elsewhere:deployment"),
        "{error}"
    );
}

#[test]
fn a_same_named_instance_without_infrastructure_shares_no_state() {
    let module = module_with_target();
    write(
        module.path(),
        "elsewhere/project.cue",
        "package deployment\n\nname: \"app\"\n",
    );
    assert!(evaluate(module.path(), NameCheck::WholeModule).is_ok());
}

#[test]
fn a_project_without_infrastructure_is_named_as_such() {
    let directory = module_directory();
    let root = directory.path();
    write(root, "cue.mod/module.cue", MODULE);
    write(root, "app/env.cue", "package cuenv\n\nname: \"app\"\n");
    let error = evaluate(root, NameCheck::TargetOnly).unwrap_err();
    assert_eq!(exit_code_for(&error), EXIT_CLI);
    assert!(
        error
            .to_string()
            .contains("project 'app' has no `infrastructure` block"),
        "{error}"
    );
}

#[test]
fn a_child_directory_inheriting_the_project_is_a_duplicate() {
    let module = module_with_target();
    write(module.path(), "app/child/env.cue", "package cuenv\n");
    let error = evaluate(module.path(), NameCheck::WholeModule).unwrap_err();
    assert!(error.to_string().contains("app/child:cuenv"), "{error}");
    // The child cannot set a name of its own in the same package; the help
    // must not suggest it.
    let help = error.help().unwrap();
    assert!(help.contains("different CUE package"), "{help}");
}

#[test]
fn an_instance_that_fails_to_evaluate_fails_closed() {
    let module = module_with_target();
    write(
        module.path(),
        "broken/env.cue",
        "package cuenv\n\nname: \"broken\"\nvalue: missingReference\n",
    );
    let error = evaluate(module.path(), NameCheck::WholeModule).unwrap_err();
    assert_eq!(exit_code_for(&error), EXIT_EVAL);
    let message = error.to_string();
    assert!(message.contains("broken:cuenv"), "{message}");
    assert!(!message.contains("CUE parsing failed"), "{message}");
    assert!(!message.contains("instanceFailures"), "{message}");
}

#[test]
fn a_project_the_loader_skips_fails_closed_and_says_why() {
    let directory = module_directory();
    write_target(directory.path(), "deploy/_staging/app");
    write(
        directory.path(),
        "other/env.cue",
        "package cuenv\n\nname: \"other\"\n",
    );
    let project = directory.path().join("deploy/_staging/app");

    let error = evaluate_at(&project, NameCheck::WholeModule).unwrap_err();
    assert_eq!(exit_code_for(&error), EXIT_CLI, "{error:?}");
    assert!(
        error
            .to_string()
            .contains("does not include this project's instance deploy/_staging/app:cuenv"),
        "{error}"
    );
    let help = error.help().unwrap();
    assert!(help.contains("'deploy/_staging' starts with '_'"), "{help}");
    assert!(!help.contains("listed above"), "{help}");

    // Inspecting and repairing state still works from there.
    assert!(evaluate_at(&project, NameCheck::TargetOnly).is_ok());
}

#[test]
fn a_skipped_project_that_is_the_only_instance_says_why() {
    let directory = module_directory();
    write_target(directory.path(), ".hidden/app");
    let error = evaluate_at(
        &directory.path().join(".hidden/app"),
        NameCheck::WholeModule,
    )
    .unwrap_err();
    let help = error.help().unwrap();
    assert!(help.contains("'.hidden' starts with '.'"), "{help}");
    assert!(!help.contains("listed above"), "{help}");
}

#[test]
fn a_module_root_named_with_a_dot_is_checked() {
    let directory = tempfile::Builder::new()
        .prefix(".cuenv-infrastructure-")
        .tempdir()
        .unwrap();
    write_target(directory.path(), "app");
    assert!(evaluate(directory.path(), NameCheck::WholeModule).is_ok());
}

#[test]
fn a_broken_sibling_does_not_block_state_commands() {
    let module = module_with_target();
    write(
        module.path(),
        "broken/env.cue",
        "package cuenv\n\nname: \"app\"\nvalue: missingReference\n",
    );
    assert!(evaluate(module.path(), NameCheck::TargetOnly).is_ok());
}

#[test]
fn the_infrastructure_block_must_be_concrete_only_for_this_command() {
    let directory = module_directory();
    let root = directory.path();
    write(root, "cue.mod/module.cue", MODULE);
    write(
        root,
        "app/env.cue",
        "package cuenv\n\nname: \"app\"\ninfrastructure: {\n\
         \tstate: turso: url: \"http://127.0.0.1:8080\"\n\
         \tresources: pet: {type: \"random_pet\", configuration: length: int}\n\
         }\n",
    );
    let error = evaluate(root, NameCheck::TargetOnly).unwrap_err();
    assert!(error.to_string().contains("infrastructure"), "{error}");

    // Other commands evaluate the same instance without the requirement.
    let executor = crate::commands::CommandExecutor::new(
        tokio::sync::mpsc::unbounded_channel().0,
        "cuenv".to_string(),
    );
    assert!(executor.get_module(&root.join("app")).is_ok());
}

// ---------------------------------------------------------------------
// Commands against a store, with injected signals and answers.
// ---------------------------------------------------------------------

impl SignalSource for mpsc::UnboundedReceiver<()> {
    async fn next(&mut self) -> bool {
        self.recv().await.is_some()
    }
}

/// A memory store counting migrations and lock acquisitions.
#[derive(Debug, Default)]
struct CountingStore {
    inner: MemoryStateStore,
    migrations: AtomicUsize,
    acquisitions: AtomicUsize,
    owner_locks: Mutex<Vec<Option<LockInformation>>>,
}

#[async_trait]
impl StateStore for CountingStore {
    async fn migrate(&self) -> cuenv_infrastructure::Result<()> {
        self.migrations.fetch_add(1, Ordering::SeqCst);
        self.inner.migrate().await
    }

    async fn list(&self, tenant: &TenantKey) -> cuenv_infrastructure::Result<Vec<ManagedResource>> {
        self.inner.list(tenant).await
    }

    async fn put(
        &self,
        tenant: &TenantKey,
        lock: &StateLock,
        resource: &ManagedResource,
    ) -> cuenv_infrastructure::Result<()> {
        self.inner.put(tenant, lock, resource).await
    }

    async fn put_if_unchanged(
        &self,
        tenant: &TenantKey,
        lock: &StateLock,
        put: &ConditionalPut<'_>,
    ) -> cuenv_infrastructure::Result<()> {
        self.inner.put_if_unchanged(tenant, lock, put).await
    }

    async fn delete(
        &self,
        tenant: &TenantKey,
        lock: &StateLock,
        address: &ResourceAddress,
    ) -> cuenv_infrastructure::Result<()> {
        self.inner.delete(tenant, lock, address).await
    }

    async fn acquire_lock(
        &self,
        tenant: &TenantKey,
        request: &LockRequest<'_>,
    ) -> cuenv_infrastructure::Result<StateLock> {
        self.acquisitions.fetch_add(1, Ordering::SeqCst);
        self.inner.acquire_lock(tenant, request).await
    }

    async fn unlock(
        &self,
        tenant: &TenantKey,
        lock: &StateLock,
    ) -> cuenv_infrastructure::Result<()> {
        self.inner.unlock(tenant, lock).await
    }

    async fn current_lock(
        &self,
        tenant: &TenantKey,
    ) -> cuenv_infrastructure::Result<Option<LockInformation>> {
        self.inner.current_lock(tenant).await
    }

    async fn force_unlock(
        &self,
        tenant: &TenantKey,
        lock_identifier: &str,
    ) -> cuenv_infrastructure::Result<bool> {
        self.inner.force_unlock(tenant, lock_identifier).await
    }

    async fn owner(&self, tenant: &TenantKey) -> cuenv_infrastructure::Result<Option<TenantOwner>> {
        let lock = self.inner.current_lock(tenant).await?;
        self.owner_locks.lock().unwrap().push(lock);
        self.inner.owner(tenant).await
    }

    async fn claim_owner(
        &self,
        tenant: &TenantKey,
        lock: &StateLock,
        claim: &OwnerClaim<'_>,
    ) -> cuenv_infrastructure::Result<TenantOwner> {
        self.inner.claim_owner(tenant, lock, claim).await
    }
}

/// What the scripted prompt does when asked.
#[derive(Debug, Clone)]
enum Script {
    /// Answer with this line.
    Line(&'static str),
    /// Standard input ends.
    EndOfInput,
    /// An interrupt arrives while the prompt waits.
    Interrupt(mpsc::UnboundedSender<()>),
}

/// Answers from a script, recording the tenant's lock when asked.
struct ScriptedAnswers {
    script: Script,
    store: Arc<dyn StateStore>,
    tenant: TenantKey,
    locks_while_asked: Mutex<Vec<Option<LockInformation>>>,
}

impl ScriptedAnswers {
    /// The tenant's lock each time the prompt was shown.
    fn locks_while_asked(&self) -> Vec<Option<LockInformation>> {
        self.locks_while_asked.lock().unwrap().clone()
    }
}

impl Answers for ScriptedAnswers {
    fn is_interactive(&self) -> bool {
        true
    }

    fn read(&self) -> AnswerFuture<'_> {
        Box::pin(async move {
            let lock = self.store.current_lock(&self.tenant).await.unwrap();
            self.locks_while_asked.lock().unwrap().push(lock);
            match &self.script {
                Script::Line(line) => Ok(Some((*line).to_string())),
                Script::EndOfInput => Ok(None),
                Script::Interrupt(signals) => {
                    signals.send(()).unwrap();
                    std::future::pending().await
                }
            }
        })
    }
}

/// Everything a command runs with, over a counting memory store.
struct Harness {
    counting: Arc<CountingStore>,
    store: Arc<dyn StateStore>,
    tenant: TenantKey,
    instance: ProjectInstance,
    output: Output,
    interrupts: Interrupts,
    answers: ScriptedAnswers,
    directory: tempfile::TempDir,
    /// Kept so the signal source stays open.
    _signals: mpsc::UnboundedSender<()>,
}

impl Harness {
    fn new(format: OutputFormat, script: Script) -> Self {
        let counting = Arc::new(CountingStore::default());
        let store: Arc<dyn StateStore> = counting.clone();
        let tenant = TenantKey::new("example.com/infrastructure", "app").unwrap();
        let output = Output::new(format);
        let (signals, receiver) = mpsc::unbounded_channel();
        let interrupts = Interrupts::watch(receiver, &output);
        let script = match script {
            // The harness's own source, so the interrupt reaches the watcher.
            Script::Interrupt(_) => Script::Interrupt(signals.clone()),
            other => other,
        };
        Self {
            counting,
            store: Arc::clone(&store),
            tenant: tenant.clone(),
            instance: ProjectInstance::new("app", "cuenv").unwrap(),
            output,
            interrupts,
            answers: ScriptedAnswers {
                script,
                store,
                tenant,
                locks_while_asked: Mutex::new(Vec::new()),
            },
            directory: module_directory(),
            _signals: signals,
        }
    }

    fn context(&self) -> CommandContext<'_> {
        CommandContext {
            store: &self.store,
            tenant: &self.tenant,
            instance: &self.instance,
            output: &self.output,
            interrupts: &self.interrupts,
            answers: &self.answers,
        }
    }

    /// Engine inputs for an `infrastructure` block without providers or
    /// resources, saving unrecorded changes in the harness's directory.
    fn inputs(&self) -> EngineInputs {
        EngineInputs {
            infrastructure: serde_json::from_value(serde_json::json!({
                "state": {"turso": {"url": "http://127.0.0.1:1"}}
            }))
            .unwrap(),
            project_directory: self.directory.path().to_path_buf(),
            unrecorded_directory: Some(self.directory.path().join("unrecorded")),
            provider_environment_variables: std::collections::BTreeMap::default(),
            withheld_environment_variables: Vec::new(),
        }
    }

    async fn run(&self, action: InfrastructureAction) -> Result<(), CliError> {
        dispatch(&action, &self.context(), self.inputs()).await
    }

    fn migrations(&self) -> usize {
        self.counting.migrations.load(Ordering::SeqCst)
    }

    fn acquisitions(&self) -> usize {
        self.counting.acquisitions.load(Ordering::SeqCst)
    }

    async fn current_lock(&self) -> Option<LockInformation> {
        self.store.current_lock(&self.tenant).await.unwrap()
    }

    /// The JSON result the command left for the end of the run.
    fn result(&self) -> serde_json::Value {
        match self.output.finish() {
            Finish::Report(Some(payload)) => payload,
            other => panic!("expected a result, got {other:?}"),
        }
    }

    /// Store records and an owner as another run would have left them.
    async fn seed(&self, resources: &[&str], owner: Option<&ProjectInstance>) {
        let lock = self.store.lock(&self.tenant, "seed").await.unwrap();
        for address in resources {
            self.store
                .put(&self.tenant, &lock, &managed(address))
                .await
                .unwrap();
        }
        if let Some(owner) = owner {
            self.store
                .claim_owner(
                    &self.tenant,
                    &lock,
                    &OwnerClaim {
                        instance: owner,
                        mode: OwnerClaimMode::Transfer,
                    },
                )
                .await
                .unwrap();
        }
        self.store.unlock(&self.tenant, &lock).await.unwrap();
    }
}

fn managed(address: &str) -> ManagedResource {
    let (resource_type, name) = address.split_once('.').unwrap();
    ManagedResource {
        address: ResourceAddress::new(resource_type, name),
        provider: "random".to_string(),
        provider_source: "registry.terraform.io/hashicorp/random".to_string(),
        schema_version: 0,
        state: serde_json::json!({"id": "x"}),
        private: Vec::new(),
        dependencies: Vec::new(),
        tainted: false,
        identity: None,
        serial: 0,
        generation: uuid::Uuid::nil(),
    }
}

fn lock_of(error: &CliError) -> Option<&LockStatus> {
    match error {
        CliError::Infrastructure { lock, .. } => lock.as_ref(),
        _ => None,
    }
}

#[tokio::test]
async fn reads_never_create_or_upgrade_tables_and_take_no_lock() {
    let harness = Harness::new(OutputFormat::Json, Script::Line("yes"));
    for action in [
        InfrastructureAction::Plan,
        InfrastructureAction::State(StateAction::List),
        InfrastructureAction::Unlock {
            lock_identifier: None,
        },
        // Naming a lock when none is held shows that and succeeds.
        InfrastructureAction::Unlock {
            lock_identifier: Some("stale".to_string()),
        },
        InfrastructureAction::State(StateAction::Recover {
            overwrite: RecoverOverwrite::IfUnchanged,
        }),
    ] {
        harness.run(action.clone()).await.unwrap();
        assert_eq!(harness.migrations(), 0, "{action:?}");
        assert_eq!(harness.acquisitions(), 0, "{action:?}");
    }
    // The last result: nothing to recover, without a lock.
    assert_eq!(harness.result()["recovered"], serde_json::json!([]));
}

#[tokio::test]
async fn writes_upgrade_the_tables_first_and_release_the_lock() {
    let harness = Harness::new(OutputFormat::Json, Script::Line("yes"));
    harness
        .seed(&["random_pet.pet", "random_integer.port"], None)
        .await;
    let seeding = harness.acquisitions();
    harness
        .run(InfrastructureAction::State(StateAction::Remove {
            address: "random_pet.pet".to_string(),
        }))
        .await
        .unwrap();
    assert_eq!(harness.migrations(), 1);
    assert_eq!(harness.acquisitions(), seeding + 1);
    let remaining: Vec<String> = harness
        .store
        .list(&harness.tenant)
        .await
        .unwrap()
        .iter()
        .map(|resource| resource.address.to_string())
        .collect();
    assert_eq!(remaining, vec!["random_integer.port".to_string()]);
    assert!(harness.current_lock().await.is_none());

    let error = harness
        .run(InfrastructureAction::State(StateAction::Remove {
            address: "random_pet.missing".to_string(),
        }))
        .await
        .unwrap_err();
    assert_eq!(exit_code_for(&error), EXIT_CLI);
    assert!(error.to_string().contains("random_pet.missing"), "{error}");
    assert!(harness.current_lock().await.is_none());
}

#[tokio::test]
async fn apply_claims_ownership_under_its_first_lock() {
    let harness = Harness::new(OutputFormat::Json, Script::Line("yes"));
    harness
        .run(InfrastructureAction::Apply {
            confirmation: ConfirmationPolicy::AssumeYes,
        })
        .await
        .unwrap();
    let owner = harness.store.owner(&harness.tenant).await.unwrap().unwrap();
    assert_eq!(owner.instance, harness.instance);
    assert!(harness.current_lock().await.is_none());
    let result = harness.result();
    assert_eq!(result["operation"], "apply");
    assert!(result["applied"].is_null());
}

#[tokio::test]
async fn nothing_to_apply_asks_nothing() {
    let harness = Harness::new(OutputFormat::Text, Script::Line("no"));
    harness
        .run(InfrastructureAction::Apply {
            confirmation: ConfirmationPolicy::Prompt,
        })
        .await
        .unwrap();
    assert!(harness.answers.locks_while_asked().is_empty());
}

#[tokio::test]
async fn another_owner_is_refused_until_adopted() {
    let harness = Harness::new(OutputFormat::Json, Script::Line("yes"));
    let other = ProjectInstance::new("_staging/app", "cuenv").unwrap();
    harness.seed(&[], Some(&other)).await;

    for action in [
        InfrastructureAction::Plan,
        InfrastructureAction::Apply {
            confirmation: ConfirmationPolicy::AssumeYes,
        },
        InfrastructureAction::Destroy {
            confirmation: ConfirmationPolicy::AssumeYes,
        },
    ] {
        let error = harness.run(action.clone()).await.unwrap_err();
        assert_eq!(exit_code_for(&error), EXIT_INFRASTRUCTURE, "{action:?}");
        assert!(error.to_string().contains("_staging/app:cuenv"), "{error}");
        assert!(error.help().unwrap().contains("state adopt"), "{error:?}");
        assert!(harness.current_lock().await.is_none(), "{action:?}");
    }
    // The owner was kept.
    assert_eq!(
        harness
            .store
            .owner(&harness.tenant)
            .await
            .unwrap()
            .unwrap()
            .instance,
        other
    );

    harness
        .run(InfrastructureAction::State(StateAction::Adopt))
        .await
        .unwrap();
    let adopted = harness.result();
    assert_eq!(adopted["previousOwner"]["instance"], "_staging/app:cuenv");
    assert_eq!(adopted["owner"]["instance"], "app:cuenv");
    harness.run(InfrastructureAction::Plan).await.unwrap();
}

#[tokio::test]
async fn state_remove_checks_the_owner_under_lock_until_adopted() {
    for environment in [None, Some("Dev")] {
        let mut harness = Harness::new(OutputFormat::Json, Script::Line("yes"));
        if let Some(environment) = environment {
            harness.tenant =
                TenantKey::with_environment("example.com/infrastructure", "app", environment)
                    .unwrap();
        }
        let other = ProjectInstance::new("_staging/app", "cuenv").unwrap();
        harness.seed(&["random_pet.pet"], Some(&other)).await;
        let before = harness.store.list(&harness.tenant).await.unwrap();
        let action = InfrastructureAction::State(StateAction::Remove {
            address: "random_pet.pet".to_string(),
        });

        let error = harness.run(action.clone()).await.unwrap_err();
        assert_eq!(exit_code_for(&error), EXIT_INFRASTRUCTURE);
        assert!(error.to_string().contains("_staging/app:cuenv"), "{error}");
        assert!(error.help().unwrap().contains("state adopt"), "{error:?}");
        let owner_lock = harness.counting.owner_locks.lock().unwrap()[0]
            .clone()
            .unwrap();
        assert_eq!(
            lock_of(&error),
            Some(&LockStatus {
                identifier: owner_lock.lock_identifier,
                released: true,
            })
        );
        assert!(harness.current_lock().await.is_none());
        assert_eq!(harness.store.list(&harness.tenant).await.unwrap(), before);
        assert_eq!(
            harness
                .store
                .owner(&harness.tenant)
                .await
                .unwrap()
                .unwrap()
                .instance,
            other
        );

        // Reading and inspecting the lock remain available from a moved instance.
        harness
            .run(InfrastructureAction::State(StateAction::List))
            .await
            .unwrap();
        harness
            .run(InfrastructureAction::Unlock {
                lock_identifier: None,
            })
            .await
            .unwrap();
        harness
            .run(InfrastructureAction::State(StateAction::Adopt))
            .await
            .unwrap();
        harness.run(action).await.unwrap();
        assert!(
            harness
                .store
                .list(&harness.tenant)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(harness.current_lock().await.is_none());
    }
}

#[tokio::test]
async fn state_recover_checks_the_owner_under_lock_even_when_forced() {
    for environment in [None, Some("Dev")] {
        let mut harness = Harness::new(OutputFormat::Json, Script::Line("yes"));
        if let Some(environment) = environment {
            harness.tenant =
                TenantKey::with_environment("example.com/infrastructure", "app", environment)
                    .unwrap();
        }
        let other = ProjectInstance::new("_staging/app", "cuenv").unwrap();
        harness.seed(&["random_pet.pet"], Some(&other)).await;
        let before = harness.store.list(&harness.tenant).await.unwrap();
        let mut saved = before[0].clone();
        saved.state = serde_json::json!({"id": "recovered"});
        let unrecorded = UnrecordedStore::at(harness.directory.path().join("unrecorded"));
        unrecorded
            .save(
                &harness.tenant,
                &ConditionalPut {
                    resource: &saved,
                    expected: RecordVersion::of(Some(&before[0])),
                },
            )
            .unwrap();
        let pending = unrecorded.list(&harness.tenant).unwrap();

        for overwrite in [RecoverOverwrite::IfUnchanged, RecoverOverwrite::Always] {
            let error = harness
                .run(InfrastructureAction::State(StateAction::Recover {
                    overwrite,
                }))
                .await
                .unwrap_err();
            assert_eq!(exit_code_for(&error), EXIT_INFRASTRUCTURE);
            assert!(error.help().unwrap().contains("state adopt"), "{error:?}");
            let owner_lock = harness
                .counting
                .owner_locks
                .lock()
                .unwrap()
                .last()
                .cloned()
                .unwrap()
                .unwrap();
            assert_eq!(
                lock_of(&error),
                Some(&LockStatus {
                    identifier: owner_lock.lock_identifier,
                    released: true,
                })
            );
            assert!(harness.current_lock().await.is_none());
            assert_eq!(harness.store.list(&harness.tenant).await.unwrap(), before);
            assert_eq!(unrecorded.list(&harness.tenant).unwrap(), pending);
        }
        assert_eq!(
            harness
                .store
                .owner(&harness.tenant)
                .await
                .unwrap()
                .unwrap()
                .instance,
            other
        );

        harness
            .run(InfrastructureAction::State(StateAction::Adopt))
            .await
            .unwrap();
        harness
            .run(InfrastructureAction::State(StateAction::Recover {
                overwrite: RecoverOverwrite::IfUnchanged,
            }))
            .await
            .unwrap();
        let recorded = harness.store.list(&harness.tenant).await.unwrap();
        assert!(recorded[0].same_content(&saved));
        assert!(unrecorded.list(&harness.tenant).unwrap().is_empty());
        assert!(harness.current_lock().await.is_none());
    }
}

#[tokio::test]
async fn declining_the_prompt_cancels_with_exit_code_one_and_releases_the_lock() {
    for (script, reason) in [
        (Script::Line("no"), "not 'yes'"),
        (Script::EndOfInput, "ended without an answer"),
    ] {
        let harness = Harness::new(OutputFormat::Text, script);
        let context = harness.context();
        let error = under_lock(&context, "apply", |_lock| async {
            confirm(cuenv_infrastructure::PlanMode::Apply, &context).await
        })
        .await
        .unwrap_err();
        assert_eq!(exit_code_for(&error), EXIT_CANCELLED);
        assert_eq!(exit_code_for(&error), 1);
        assert_eq!(error_code_for(&error), "infrastructure_cancelled");
        assert!(error.to_string().contains(reason), "{error}");
        // The prompt was answered while the lock was held, and the lock is
        // released afterwards.
        let held = harness.answers.locks_while_asked()[0].clone().unwrap();
        assert!(
            held.holder.starts_with("cuenv infrastructure apply by "),
            "{}",
            held.holder
        );
        assert_eq!(
            lock_of(&error),
            Some(&LockStatus {
                identifier: held.lock_identifier,
                released: true
            })
        );
        assert!(harness.current_lock().await.is_none());
    }
}

#[tokio::test]
async fn confirming_the_prompt_proceeds_under_the_same_lock() {
    let harness = Harness::new(OutputFormat::Text, Script::Line("yes"));
    let context = harness.context();
    let lock = under_lock(&context, "apply", |lock| async {
        confirm(cuenv_infrastructure::PlanMode::Apply, &context).await?;
        Ok(lock)
    })
    .await
    .unwrap();
    let held = harness.answers.locks_while_asked()[0].clone().unwrap();
    assert_eq!(held.lock_identifier, lock.lock_identifier);
    assert!(harness.current_lock().await.is_none());
}

#[tokio::test]
async fn an_interrupt_at_the_prompt_releases_the_lock() {
    let harness = Harness::new(
        OutputFormat::Text,
        Script::Interrupt(mpsc::unbounded_channel().0),
    );
    let context = harness.context();
    let error = under_lock(&context, "destroy", |_lock| async {
        confirm(cuenv_infrastructure::PlanMode::Destroy, &context).await
    })
    .await
    .unwrap_err();
    assert_eq!(exit_code_for(&error), EXIT_INTERRUPTED);
    assert!(lock_of(&error).unwrap().released);
    assert!(harness.answers.locks_while_asked()[0].is_some());
    assert!(harness.current_lock().await.is_none());
}

#[tokio::test]
async fn state_recover_records_and_deletes_unrecorded_files() {
    let harness = Harness::new(OutputFormat::Json, Script::Line("yes"));
    let unrecorded = UnrecordedStore::at(harness.directory.path().join("unrecorded"));
    unrecorded
        .save(
            &harness.tenant,
            &ConditionalPut {
                resource: &managed("random_pet.pet"),
                expected: RecordVersion::Absent,
            },
        )
        .unwrap();

    harness
        .run(InfrastructureAction::State(StateAction::Recover {
            overwrite: RecoverOverwrite::IfUnchanged,
        }))
        .await
        .unwrap();
    let recorded: Vec<String> = harness
        .store
        .list(&harness.tenant)
        .await
        .unwrap()
        .iter()
        .map(|resource| resource.address.to_string())
        .collect();
    assert_eq!(recorded, vec!["random_pet.pet".to_string()]);
    assert!(unrecorded.list(&harness.tenant).unwrap().is_empty());
    assert!(harness.current_lock().await.is_none());
    assert_eq!(
        harness.result()["recovered"],
        serde_json::json!(["random_pet.pet"])
    );
}

#[tokio::test]
async fn state_recover_refuses_to_overwrite_a_newer_record_unless_forced() {
    let harness = Harness::new(OutputFormat::Json, Script::Line("yes"));
    let unrecorded = UnrecordedStore::at(harness.directory.path().join("unrecorded"));
    unrecorded
        .save(
            &harness.tenant,
            &ConditionalPut {
                resource: &managed("random_pet.pet"),
                expected: RecordVersion::Absent,
            },
        )
        .unwrap();
    // Another run recorded the resource since the change was saved.
    let mut newer = managed("random_pet.pet");
    newer.state = serde_json::json!({"id": "newer"});
    let lock = harness.store.lock(&harness.tenant, "other").await.unwrap();
    harness
        .store
        .put(&harness.tenant, &lock, &newer)
        .await
        .unwrap();
    harness.store.unlock(&harness.tenant, &lock).await.unwrap();

    let error = harness
        .run(InfrastructureAction::State(StateAction::Recover {
            overwrite: RecoverOverwrite::IfUnchanged,
        }))
        .await
        .unwrap_err();
    assert_eq!(exit_code_for(&error), EXIT_INFRASTRUCTURE);
    let help = error.help().unwrap();
    assert!(help.contains("state recover --force"), "{help}");
    assert!(help.contains("move its file"), "{help}");
    assert!(!help.contains("Turso"), "{help}");
    assert_eq!(unrecorded.list(&harness.tenant).unwrap().len(), 1);
    assert!(harness.current_lock().await.is_none());

    harness
        .run(InfrastructureAction::State(StateAction::Recover {
            overwrite: RecoverOverwrite::Always,
        }))
        .await
        .unwrap();
    let stored = harness.store.list(&harness.tenant).await.unwrap();
    assert_eq!(stored[0].state, serde_json::json!({"id": "x"}));
    assert!(unrecorded.list(&harness.tenant).unwrap().is_empty());
}

#[tokio::test]
async fn an_unusable_unrecorded_file_names_the_file_not_the_state_store() {
    let harness = Harness::new(OutputFormat::Json, Script::Line("yes"));
    let unrecorded = UnrecordedStore::at(harness.directory.path().join("unrecorded"));
    unrecorded
        .save(
            &harness.tenant,
            &ConditionalPut {
                resource: &managed("random_pet.pet"),
                expected: RecordVersion::Absent,
            },
        )
        .unwrap();
    let file = unrecorded.list(&harness.tenant).unwrap()[0].file.clone();
    std::fs::write(&file, "{not json").unwrap();

    let error = harness
        .run(InfrastructureAction::State(StateAction::Recover {
            overwrite: RecoverOverwrite::IfUnchanged,
        }))
        .await
        .unwrap_err();
    assert_eq!(exit_code_for(&error), EXIT_INFRASTRUCTURE);
    let help = error.help().unwrap();
    assert!(help.contains(&file.display().to_string()), "{help}");
    assert!(help.contains("move it out of that directory"), "{help}");
    assert!(!help.contains("Turso"), "{help}");
}

#[tokio::test]
async fn unlock_with_another_identifier_is_a_lock_failure() {
    let harness = Harness::new(OutputFormat::Text, Script::Line("yes"));
    let lock = harness
        .store
        .lock(&harness.tenant, "someone")
        .await
        .unwrap();
    let error = harness
        .run(InfrastructureAction::Unlock {
            lock_identifier: Some("stale".to_string()),
        })
        .await
        .unwrap_err();
    assert_eq!(exit_code_for(&error), EXIT_LOCKED);
    assert_eq!(error_code_for(&error), "infrastructure_locked");
    assert_eq!(
        lock_of(&error),
        Some(&LockStatus {
            identifier: lock.lock_identifier.clone(),
            released: false
        })
    );
    assert!(harness.current_lock().await.is_some());
}

/// A store whose lock is free and whose unlock always fails, counting
/// attempts.
#[derive(Debug, Default)]
struct UnreleasableStore {
    unlock_attempts: AtomicUsize,
}

#[async_trait]
impl StateStore for UnreleasableStore {
    async fn migrate(&self) -> cuenv_infrastructure::Result<()> {
        Ok(())
    }

    async fn list(
        &self,
        _tenant: &TenantKey,
    ) -> cuenv_infrastructure::Result<Vec<ManagedResource>> {
        Ok(Vec::new())
    }

    async fn put(
        &self,
        _tenant: &TenantKey,
        _lock: &StateLock,
        _resource: &ManagedResource,
    ) -> cuenv_infrastructure::Result<()> {
        Ok(())
    }

    async fn put_if_unchanged(
        &self,
        _tenant: &TenantKey,
        _lock: &StateLock,
        _put: &ConditionalPut<'_>,
    ) -> cuenv_infrastructure::Result<()> {
        Ok(())
    }

    async fn delete(
        &self,
        _tenant: &TenantKey,
        _lock: &StateLock,
        _address: &ResourceAddress,
    ) -> cuenv_infrastructure::Result<()> {
        Ok(())
    }

    async fn acquire_lock(
        &self,
        _tenant: &TenantKey,
        request: &LockRequest<'_>,
    ) -> cuenv_infrastructure::Result<StateLock> {
        Ok(request.lock.clone())
    }

    async fn owner(
        &self,
        _tenant: &TenantKey,
    ) -> cuenv_infrastructure::Result<Option<TenantOwner>> {
        Ok(None)
    }

    async fn claim_owner(
        &self,
        _tenant: &TenantKey,
        _lock: &StateLock,
        claim: &OwnerClaim<'_>,
    ) -> cuenv_infrastructure::Result<TenantOwner> {
        Ok(TenantOwner {
            instance: claim.instance.clone(),
            claimed_at: "2026-09-28T00:00:00Z".to_string(),
        })
    }

    async fn unlock(
        &self,
        _tenant: &TenantKey,
        _lock: &StateLock,
    ) -> cuenv_infrastructure::Result<()> {
        self.unlock_attempts.fetch_add(1, Ordering::SeqCst);
        Err(InfrastructureError::state("unavailable"))
    }

    async fn current_lock(
        &self,
        _tenant: &TenantKey,
    ) -> cuenv_infrastructure::Result<Option<LockInformation>> {
        Ok(None)
    }

    async fn force_unlock(
        &self,
        _tenant: &TenantKey,
        _lock_identifier: &str,
    ) -> cuenv_infrastructure::Result<bool> {
        Ok(false)
    }
}

#[tokio::test(start_paused = true)]
async fn release_does_not_wait_after_its_last_attempt() {
    let store = UnreleasableStore::default();
    let tenant = TenantKey::new("example.com/infrastructure", "app").unwrap();
    let started = tokio::time::Instant::now();
    assert!(
        release(&store, &tenant, &StateLock::generate())
            .await
            .is_err()
    );
    assert_eq!(store.unlock_attempts.load(Ordering::SeqCst), 3);
    // Time is paused and advances only through the waits: 250 ms and
    // 500 ms between the three attempts, and none after the last.
    assert_eq!(started.elapsed().as_millis(), 750);
}

#[tokio::test(start_paused = true)]
async fn an_unreleased_lock_is_never_reported_as_released() {
    let mut harness = Harness::new(OutputFormat::Json, Script::Line("yes"));
    harness.store = Arc::new(UnreleasableStore::default());
    let context = harness.context();

    // An interrupted run whose release failed.
    let error = under_lock(&context, "apply", |_lock| async {
        Err::<(), _>(super::interrupts::interrupted())
    })
    .await
    .unwrap_err();
    assert_eq!(exit_code_for(&error), EXIT_INTERRUPTED);
    let lock = lock_of(&error).unwrap();
    assert!(!lock.released);
    let help = error.help().unwrap();
    assert!(help.contains("NOT released"), "{help}");
    assert!(
        help.contains(&format!("cuenv infrastructure unlock {}", lock.identifier)),
        "{help}"
    );

    // A successful run whose release failed.
    let error = under_lock(&context, "state remove", |_lock| async { Ok(()) })
        .await
        .unwrap_err();
    assert_eq!(exit_code_for(&error), EXIT_INFRASTRUCTURE);
    assert!(!lock_of(&error).unwrap().released);
    assert!(error.to_string().contains("was NOT released"), "{error}");

    // A configuration error whose release failed keeps the lock in the
    // report.
    let error = under_lock(&context, "state remove", |_lock| async {
        Err::<(), _>(CliError::config("no such resource"))
    })
    .await
    .unwrap_err();
    assert!(error.to_string().contains("no such resource"), "{error}");
    assert!(!lock_of(&error).unwrap().released);
}
