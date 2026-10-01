use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use cuenv_infrastructure::{
    ChangedRecord, ConditionalPut, InfrastructureError, LockInformation, LockRequest,
    ManagedResource, MemoryStateStore, OwnerClaim, OwnerClaimMode, ProjectInstance, RecordVersion,
    RecoverOverrides, ResourceAddress, StateLock, StateStore, TenantKey, TenantOwner,
    UnrecordedStore,
};
use cuenv_manifest::environment::EnvValue;
use tokio::sync::mpsc;

use super::evaluation::{self, NameCheck, Needs, TargetRequest};
use super::interrupts::{Interrupts, SignalSource};
use super::invocation::Invocation;
use super::output::{Finish, Output};
use super::{
    AnswerFuture, Answers, CommandContext, ConfirmationPolicy, EngineInputs, InfrastructureAction,
    InfrastructureOptions, Siblings, StateAction, confirm, dispatch,
    environment_variables_for_action, release, run, under_lock,
};
use crate::cli::{
    CliError, EXIT_CANCELLED, EXIT_CLI, EXIT_EVAL, EXIT_INFRASTRUCTURE, EXIT_INTERRUPTED,
    EXIT_LOCKED, EXIT_OK, LockStatus, OutputFormat, error_code_for, exit_code_for,
};

/// Recovery that overwrites a stored record that changed (`--force`).
fn overwrite_changed() -> RecoverOverrides {
    RecoverOverrides {
        changed_record: ChangedRecord::Overwrite,
        ..RecoverOverrides::default()
    }
}

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
        needs: Needs::Configuration,
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
        target
            .project_environment
            .as_ref()
            .unwrap()
            .for_environment("dev")["BASE"]
            .to_string_value(),
        "development"
    );
    assert!(
        !target
            .project_environment
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
    siblings: Siblings,
    invocation: Invocation,
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
        let interrupts = Interrupts::watch(receiver, &output, Invocation::default());
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
            siblings: Siblings {
                unselected_tenant: tenant.clone(),
                declared_environments: Vec::new(),
                selected: None,
            },
            invocation: Invocation::default(),
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
            siblings: &self.siblings,
            invocation: &self.invocation,
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
        InfrastructureAction::State(StateAction::Recover {
            overrides: RecoverOverrides::default(),
        }),
    ] {
        harness.run(action.clone()).await.unwrap();
        assert_eq!(harness.migrations(), 0, "{action:?}");
        assert_eq!(harness.acquisitions(), 0, "{action:?}");
    }
    // The last result: nothing to recover, without a lock.
    assert_eq!(harness.result()["recovered"], serde_json::json!([]));

    // Naming a lock when none is held is an error (it matched nothing), and
    // still reads only.
    let error = harness
        .run(InfrastructureAction::Unlock {
            lock_identifier: Some("stale".to_string()),
        })
        .await
        .unwrap_err();
    assert_eq!(exit_code_for(&error), EXIT_CLI);
    assert_eq!(harness.migrations(), 0);
    assert_eq!(harness.acquisitions(), 0);
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

        for overrides in [RecoverOverrides::default(), overwrite_changed()] {
            let error = harness
                .run(InfrastructureAction::State(StateAction::Recover {
                    overrides,
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
                overrides: RecoverOverrides::default(),
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
            overrides: RecoverOverrides::default(),
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
            overrides: RecoverOverrides::default(),
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
            overrides: overwrite_changed(),
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
            overrides: RecoverOverrides::default(),
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

// ---------------------------------------------------------------------------
// Milestone 5: preflight order, identities, state-only evaluation, hints.
// ---------------------------------------------------------------------------

/// A module whose project declares a secret that, when resolved, leaves
/// `marker` behind, and the given `infrastructure` block.
fn module_with_marker_secret(marker: &Path, infrastructure: &str) -> tempfile::TempDir {
    let module = module_directory();
    write(module.path(), "cue.mod/module.cue", MODULE);
    write(
        module.path(),
        "app/env.cue",
        &format!(
            r#"package cuenv
name: "app"
env: SECRET_TOKEN: {{resolver: "exec", command: "/bin/sh", args: ["-c", "touch {} && echo value-1234"]}}
{infrastructure}"#,
            marker.display()
        ),
    );
    module
}

fn options_for(module: &Path, action: InfrastructureAction) -> InfrastructureOptions {
    InfrastructureOptions {
        path: module.join("app").to_string_lossy().into_owned(),
        package: "cuenv".into(),
        environment: None,
        action,
        output: OutputFormat::Text,
    }
}

#[tokio::test]
async fn an_invalid_state_url_fails_before_any_secret_resolver_runs_for_every_action() {
    let scratch = module_directory();
    let marker = scratch.path().join("resolver-ran");
    let module = module_with_marker_secret(
        &marker,
        r#"infrastructure: {
  state: turso: url: "http://example.invalid:8080"
  providers: random: {source: "hashicorp/random", version: "3.7.2"}
  resources: pet: {type: "random_pet", configuration: length: 2}
}
"#,
    );
    for action in [
        InfrastructureAction::Plan,
        InfrastructureAction::Apply {
            confirmation: ConfirmationPolicy::AssumeYes,
        },
        InfrastructureAction::Destroy {
            confirmation: ConfirmationPolicy::AssumeYes,
        },
        InfrastructureAction::State(StateAction::List),
        InfrastructureAction::State(StateAction::Remove {
            address: "random_pet.pet".to_string(),
        }),
        InfrastructureAction::State(StateAction::Recover {
            overrides: RecoverOverrides::default(),
        }),
        InfrastructureAction::State(StateAction::Adopt),
        InfrastructureAction::Unlock {
            lock_identifier: None,
        },
    ] {
        let harness = Harness::new(OutputFormat::Text, Script::Line("yes"));
        let error = run(
            &options_for(module.path(), action.clone()),
            &harness.output,
            &harness.interrupts,
        )
        .await
        .unwrap_err();
        assert_eq!(exit_code_for(&error), EXIT_CLI, "{action:?}: {error}");
        assert!(
            error.to_string().to_lowercase().contains("loopback"),
            "{action:?}: expected the URL contract, got {error}"
        );
        assert!(
            !marker.exists(),
            "{action:?}: the secret resolver ran before the URL was validated"
        );
    }
}

fn module_with_environments(top_level_resources: bool) -> tempfile::TempDir {
    let module = module_directory();
    write(module.path(), "cue.mod/module.cue", MODULE);
    let top_level = if top_level_resources {
        "  providers: random: {source: \"hashicorp/random\", version: \"3.7.2\"}\n  \
         resources: pet: {type: \"random_pet\", configuration: length: 2}\n"
    } else {
        "  providers: random: {source: \"hashicorp/random\", version: \"3.7.2\"}\n"
    };
    write(
        module.path(),
        "app/env.cue",
        &format!(
            r#"package cuenv
name: "app"
infrastructure: {{
  state: turso: url: "http://127.0.0.1:8080"
{top_level}  environments: {{
    dev: {{
      providers: random: {{source: "hashicorp/random", version: "3.7.2"}}
      resources: pet: {{type: "random_pet", configuration: length: 2}}
    }}
    prod: {{
      providers: random: {{source: "hashicorp/random", version: "3.7.2"}}
      resources: pet: {{type: "random_pet", configuration: length: 4}}
    }}
  }}
}}
"#
        ),
    );
    module
}

#[tokio::test]
async fn a_run_without_env_is_refused_when_only_environments_are_declared() {
    let module = module_with_environments(false);
    for action in [
        InfrastructureAction::Plan,
        InfrastructureAction::Apply {
            confirmation: ConfirmationPolicy::AssumeYes,
        },
        InfrastructureAction::Destroy {
            confirmation: ConfirmationPolicy::AssumeYes,
        },
    ] {
        let harness = Harness::new(OutputFormat::Text, Script::Line("yes"));
        let error = run(
            &options_for(module.path(), action.clone()),
            &harness.output,
            &harness.interrupts,
        )
        .await
        .unwrap_err();
        assert_eq!(exit_code_for(&error), EXIT_CLI, "{action:?}: {error}");
        let text = format!("{error} {}", error.help().unwrap_or_default());
        assert!(text.contains("--env"), "{action:?}: {text}");
        assert!(text.contains("declared environments: dev, prod"), "{text}");
        assert!(text.contains("--env dev"), "an example to copy: {text}");
        assert_eq!(harness.acquisitions(), 0);
    }
}

#[test]
fn a_project_with_top_level_resources_still_runs_without_env() {
    let module = module_with_environments(true);
    let target = evaluate_at(&module.path().join("app"), NameCheck::TargetOnly).unwrap();
    assert_eq!(target.declared_environments, vec!["dev", "prod"]);
    assert!(!target.infrastructure.resources.is_empty());
    let invocation = Invocation::default();
    assert!(super::refuse_unselected_environment(&target, &invocation).is_ok());
}

#[tokio::test]
async fn a_named_environment_without_state_is_refused_while_the_unselected_identity_has_state() {
    let harness = Harness::new(OutputFormat::Text, Script::Line("yes"));
    harness
        .seed(&["random_pet.pet", "random_id.id"], None)
        .await;
    let named = TenantKey::with_environment("example.com/infrastructure", "app", "dev").unwrap();
    let siblings = Siblings {
        unselected_tenant: harness.tenant.clone(),
        declared_environments: vec!["dev".to_string()],
        selected: Some("dev".to_string()),
    };
    let invocation = Invocation::default().with_environment(Some("dev"));
    let context = CommandContext {
        tenant: &named,
        siblings: &siblings,
        invocation: &invocation,
        ..harness.context()
    };
    let error = super::refuse_unmoved_state(&context).await.unwrap_err();
    assert_eq!(exit_code_for(&error), EXIT_CLI);
    let text = format!("{error} {}", error.help().unwrap());
    assert!(text.contains("2 resource(s)"), "{text}");
    assert!(text.contains("without --env"), "{text}");
    assert!(text.contains("`state move`"), "{text}");
    assert!(text.contains("is not available yet"), "{text}");
    // The hints act on the identity that holds the records.
    assert!(
        text.contains("`cuenv infrastructure destroy`"),
        "no --env on the legacy destroy: {text}"
    );

    // Once the named environment has state of its own, nothing is refused.
    let lock = harness.store.lock(&named, "seed").await.unwrap();
    harness
        .store
        .put(&named, &lock, &managed("random_pet.pet"))
        .await
        .unwrap();
    harness.store.unlock(&named, &lock).await.unwrap();
    assert!(super::refuse_unmoved_state(&context).await.is_ok());
}

#[tokio::test]
async fn nothing_is_refused_when_neither_identity_has_state_or_no_environment_is_selected() {
    let harness = Harness::new(OutputFormat::Text, Script::Line("yes"));
    // No environment selected: the check does not apply.
    assert!(
        super::refuse_unmoved_state(&harness.context())
            .await
            .is_ok()
    );
    // An environment selected, but no records anywhere.
    let named = TenantKey::with_environment("example.com/infrastructure", "app", "dev").unwrap();
    let context = CommandContext {
        tenant: &named,
        ..harness.context()
    };
    assert!(super::refuse_unmoved_state(&context).await.is_ok());
}

#[test]
fn an_unknown_environment_lists_the_declared_names_and_escapes_the_typed_one() {
    let module = module_with_environments(true);
    let project = module.path().join("app");
    let error =
        evaluate_at_environment(&project, NameCheck::TargetOnly, Some("stage")).unwrap_err();
    let text = format!("{error} {}", error.help().unwrap_or_default());
    assert!(
        text.contains("no infrastructure environment named 'stage'"),
        "{text}"
    );
    assert!(
        text.to_lowercase()
            .contains("declared environments: dev, prod"),
        "{text}"
    );
    assert!(
        text.contains("not inherited"),
        "top-level is not inherited: {text}"
    );

    let error = evaluate_at_environment(&project, NameCheck::TargetOnly, Some("a\u{1b}[31mred"))
        .unwrap_err();
    let text = format!("{error} {}", error.help().unwrap_or_default());
    assert!(!text.contains('\u{1b}'), "raw escape in {text:?}");
    assert!(text.contains("a\\u{1b}[31mred"), "{text}");
}

#[test]
fn a_project_declaring_no_environments_says_so() {
    let module = module_with_target();
    let error = evaluate_at_environment(
        &module.path().join("app"),
        NameCheck::TargetOnly,
        Some("dev"),
    )
    .unwrap_err();
    let text = format!("{error} {}", error.help().unwrap_or_default());
    assert!(
        text.contains("declares no infrastructure environments"),
        "{text}"
    );
}

fn evaluate_state_only(project: &Path, environment: Option<&str>) -> evaluation::Target {
    evaluation::evaluate(TargetRequest {
        path: project.to_str().unwrap(),
        package: "cuenv",
        name_check: NameCheck::TargetOnly,
        needs: Needs::StateOnly,
        environment,
    })
    .unwrap()
}

#[test]
fn state_only_evaluation_needs_nothing_but_the_state_backend() {
    let module = module_directory();
    write(module.path(), "cue.mod/module.cue", MODULE);
    write(
        module.path(),
        "app/env.cue",
        r#"package cuenv
name: "app"
infrastructure: {
  state: turso: url: "http://127.0.0.1:8080"
  // Not concrete: planning could not use this, listing state can.
  providers: random: {source: "hashicorp/random", version: string}
  resources: pet: {type: "random_pet", configuration: length: int}
  environments: dev: providers: random: {source: "hashicorp/random", version: _}
}
"#,
    );
    let project = module.path().join("app");
    // The configuration is incomplete: a command that needs it fails ...
    assert!(evaluate_at(&project, NameCheck::TargetOnly).is_err());
    assert!(evaluate_at_environment(&project, NameCheck::TargetOnly, Some("dev")).is_err());
    // ... and a state-only command does not.
    let target = evaluate_state_only(&project, None);
    assert_eq!(
        target.infrastructure.state.turso.url,
        "http://127.0.0.1:8080"
    );
    assert!(target.infrastructure.providers.is_empty());
    assert!(target.warnings.is_empty());
    let target = evaluate_state_only(&project, Some("dev"));
    assert_eq!(
        target.tenant,
        TenantKey::with_environment("example.com/infrastructure", "app", "dev").unwrap()
    );
    assert!(target.warnings.is_empty());
}

#[test]
fn state_only_evaluation_tolerates_a_removed_environment_with_a_warning() {
    let module = module_with_environments(true);
    let target = evaluate_state_only(&module.path().join("app"), Some("retired"));
    assert_eq!(
        target.tenant,
        TenantKey::with_environment("example.com/infrastructure", "app", "retired").unwrap()
    );
    assert_eq!(target.warnings.len(), 1, "{:?}", target.warnings);
    let warning = &target.warnings[0];
    assert!(warning.contains("no longer declares"), "{warning}");
    assert!(warning.contains("'retired'"), "{warning}");
    assert!(
        warning.contains("declared environments: dev, prod"),
        "{warning}"
    );
}

fn state_changed() -> InfrastructureError {
    InfrastructureError::StateChanged {
        address: "random_pet.pet".to_string(),
        expected: "1".to_string(),
        found: "2".to_string(),
        file: None,
    }
}

#[test]
fn every_repair_hint_carries_the_selected_environment_and_project() {
    let invocation = Invocation::of(&InfrastructureOptions {
        path: "./services/api".to_string(),
        package: "ops".to_string(),
        environment: Some("dev".to_string()),
        action: InfrastructureAction::Plan,
        output: OutputFormat::Text,
    });
    let flags = "--env dev -p ./services/api --package ops";
    let help = super::failure(&state_changed(), &invocation)
        .help()
        .unwrap()
        .to_string();
    assert!(
        help.contains(&format!(
            "cuenv infrastructure state recover --force {flags}"
        )),
        "{help}"
    );
    let pending = InfrastructureError::UnrecordedChangesPending {
        tenant: "example.com/infrastructure#app@dev".to_string(),
        count: 1,
        directory: "/state/x".to_string(),
    };
    let error = super::failure(&pending, &invocation);
    let text = format!("{error} {}", error.help().unwrap());
    assert!(
        text.contains(&format!("cuenv infrastructure state recover {flags}")),
        "{text}"
    );
    assert!(
        !text.contains("`cuenv infrastructure state recover`"),
        "a hint without the flags: {text}"
    );
    let owned = InfrastructureError::OwnedByAnotherInstance {
        tenant: "t".to_string(),
        owner: "a:cuenv".to_string(),
        instance: "b:cuenv".to_string(),
    };
    let error = super::failure(&owned, &invocation);
    let text = format!("{error} {}", error.help().unwrap());
    assert!(
        text.contains(&format!("cuenv infrastructure state adopt {flags}")),
        "{text}"
    );
    let lost = InfrastructureError::LockLost {
        tenant: "t".to_string(),
        lock_identifier: "abc".to_string(),
    };
    let help = super::failure(&lost, &invocation)
        .help()
        .unwrap()
        .to_string();
    assert!(
        help.contains(&format!("cuenv infrastructure plan {flags}")),
        "{help}"
    );
}

#[tokio::test(start_paused = true)]
async fn an_unreleased_lock_hint_carries_the_environment() {
    let harness = Harness::new(OutputFormat::Text, Script::Line("yes"));
    let invocation = Invocation::default().with_environment(Some("dev"));
    let store: Arc<dyn StateStore> = Arc::new(UnreleasableStore::default());
    let context = CommandContext {
        invocation: &invocation,
        store: &store,
        ..harness.context()
    };
    let error = under_lock(&context, "state remove", |_lock| async { Ok(()) })
        .await
        .unwrap_err();
    let help = error.help().unwrap();
    assert!(
        help.contains("`cuenv infrastructure unlock ") && help.contains(" --env dev`"),
        "{help}"
    );
}

#[tokio::test]
async fn unlock_with_an_identifier_that_matches_no_lock_fails_and_names_where_it_is() {
    let harness = Harness::new(OutputFormat::Text, Script::Line("yes"));
    let dev = TenantKey::with_environment("example.com/infrastructure", "app", "dev").unwrap();
    let lock = harness.store.lock(&dev, "someone").await.unwrap();
    let siblings = Siblings {
        unselected_tenant: harness.tenant.clone(),
        declared_environments: vec!["dev".to_string()],
        selected: None,
    };
    let context = CommandContext {
        siblings: &siblings,
        ..harness.context()
    };
    // Nothing is locked here, and the named lock is the dev environment's.
    let error = dispatch(
        &InfrastructureAction::Unlock {
            lock_identifier: Some(lock.lock_identifier.clone()),
        },
        &context,
        harness.inputs(),
    )
    .await
    .unwrap_err();
    assert_ne!(exit_code_for(&error), EXIT_OK);
    let text = format!("{error} {}", error.help().unwrap());
    assert!(
        text.contains(&format!("unlock {} --env dev", lock.lock_identifier)),
        "{text}"
    );
    assert!(
        harness.store.current_lock(&dev).await.unwrap().is_some(),
        "the other environment's lock is untouched"
    );

    // An identifier that is nowhere also fails.
    let error = dispatch(
        &InfrastructureAction::Unlock {
            lock_identifier: Some("nowhere".to_string()),
        },
        &context,
        harness.inputs(),
    )
    .await
    .unwrap_err();
    assert_ne!(exit_code_for(&error), EXIT_OK);
    assert!(error.to_string().contains("nowhere"), "{error}");
}

#[tokio::test]
async fn showing_the_lock_without_env_mentions_locks_in_declared_environments() {
    let harness = Harness::new(OutputFormat::Text, Script::Line("yes"));
    let dev = TenantKey::with_environment("example.com/infrastructure", "app", "dev").unwrap();
    let lock = harness.store.lock(&dev, "ci-run").await.unwrap();
    let siblings = Siblings {
        unselected_tenant: harness.tenant.clone(),
        declared_environments: vec!["dev".to_string(), "prod".to_string()],
        selected: None,
    };
    let context = CommandContext {
        siblings: &siblings,
        ..harness.context()
    };
    let notes = super::locks_in_other_environments(&context).await;
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(
        notes[0].contains("environment 'dev' is locked by 'ci-run'"),
        "{notes:?}"
    );
    assert!(
        notes[0].contains(&format!("unlock {} --env dev", lock.lock_identifier)),
        "{notes:?}"
    );
    // With --env selected, nothing else is mentioned.
    let siblings = Siblings {
        selected: Some("dev".to_string()),
        ..siblings
    };
    let context = CommandContext {
        siblings: &siblings,
        ..harness.context()
    };
    assert!(
        super::locks_in_other_environments(&context)
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn recovering_without_env_mentions_pending_changes_of_declared_environments() {
    let harness = Harness::new(OutputFormat::Text, Script::Line("yes"));
    let unrecorded = UnrecordedStore::at(harness.directory.path().join("unrecorded"));
    let dev = TenantKey::with_environment("example.com/infrastructure", "app", "dev").unwrap();
    unrecorded
        .save(
            &dev,
            &cuenv_infrastructure::ConditionalPut {
                resource: &managed("random_pet.pet"),
                expected: RecordVersion::Absent,
            },
        )
        .unwrap();
    let siblings = Siblings {
        unselected_tenant: harness.tenant.clone(),
        declared_environments: vec!["dev".to_string(), "prod".to_string()],
        selected: None,
    };
    let context = CommandContext {
        siblings: &siblings,
        ..harness.context()
    };
    let notes = super::pending_in_other_environments(&context, &unrecorded);
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(notes[0].contains("environment 'dev'"), "{notes:?}");
    assert!(
        notes[0].contains("cuenv infrastructure state recover --env dev"),
        "{notes:?}"
    );
}

#[test]
fn json_results_carry_the_environment_in_its_own_field() {
    let output = Output::new(OutputFormat::Json);
    let dev = TenantKey::with_environment("example.com/infrastructure", "app", "dev").unwrap();
    output.state(&dev, &[]);
    let Finish::Report(Some(payload)) = output.finish() else {
        panic!("expected a result");
    };
    assert_eq!(payload["tenant"], "example.com/infrastructure#app");
    assert_eq!(payload["environment"], "dev");

    let output = Output::new(OutputFormat::Json);
    let unselected = TenantKey::new("example.com/infrastructure", "app").unwrap();
    output.state(&unselected, &[]);
    let Finish::Report(Some(payload)) = output.finish() else {
        panic!("expected a result");
    };
    assert_eq!(payload["tenant"], "example.com/infrastructure#app");
    assert!(payload["environment"].is_null(), "{payload}");
}

#[tokio::test]
async fn a_failing_secret_names_its_variable_and_never_echoes_the_command_output() {
    let variables = HashMap::from([
        (
            "OK_VARIABLE".to_string(),
            EnvValue::String("plain".to_string()),
        ),
        (
            "DB_PASSWORD".to_string(),
            EnvValue::Secret(cuenv_manifest::secrets::Secret::new(
                "/bin/sh".to_string(),
                vec![
                    "-c".to_string(),
                    "echo leaked-token-VALUE-77 >&2; exit 7".to_string(),
                ],
            )),
        ),
    ]);
    let error = super::resolve_environment_variables("plan", &variables)
        .await
        .unwrap_err();
    let text = format!("{error} {}", error.help().unwrap_or_default());
    assert!(text.contains("DB_PASSWORD"), "{text}");
    assert!(!text.contains("leaked-token"), "{text}");
    assert!(!text.contains("'secret'"), "{text}");
    assert_eq!(exit_code_for(&error), EXIT_EVAL);
}
