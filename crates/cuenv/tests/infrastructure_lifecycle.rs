//! Random provider lifecycle through the CLI, checked-out CUE example and Turso.
//!
//! Two tests run the CLI against a real provider and state server: the legacy
//! no-flag stack from the checked-out example, and a named `--env dev` stack
//! whose provider receives an `#ExecSecret` through `allowInfrastructure`.
//!
//! These tests are ignored because they need a real provider binary and state server:
//! ```text
//! CUENV_INFRASTRUCTURE_TEST_RANDOM_PROVIDER=/path/terraform-provider-random \
//! CUENV_INFRASTRUCTURE_TEST_SQLD=/path/sqld \
//! cuenv exec -- cargo test -p cuenv --test infrastructure_lifecycle -- --ignored
//! ```
//! Each test starts its own server. To use a database instead, set
//! `CUENV_INFRASTRUCTURE_TEST_TURSO_URL`, and `TURSO_AUTH_TOKEN` if it is
//! authenticated: every generated configuration passes it through as the state
//! token. Each run uses a unique project name and temporary configuration and
//! user directories.
//! Data source tests additionally use `CUENV_INFRASTRUCTURE_TEST_FAKE_PROVIDER`.

use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Mutex;

use cuenv_infrastructure::{StateStore, TenantKey, TursoConfiguration, TursoStateStore};
use serde_json::Value;

#[path = "../../infrastructure/tests/support/sqld.rs"]
mod sqld;
use sqld::Database;

type TestResult<Success = ()> = Result<Success, Box<dyn Error>>;

const MODULE: &str = "github.com/cuenv/cuenv";
const PET_LENGTH: &str = "length:    2";
const SECRET_VARIABLE: &str = "DEPLOY_TOKEN";
const SECRET_VALUE: &str = "lifecycle-secret-4f9c2d71-never-printed";
const RECORDED_SECRET: &str = "recorded-secret";
const CACHE_TOKEN_VARIABLE: &str = "LIFECYCLE_CACHE_TOKEN";
const CACHE_TOKEN_VALUE: &str = "lifecycle-cache-token-7b1e90-never-passed";

fn write_executable(path: &Path, contents: &str) -> TestResult {
    use std::os::unix::fs::PermissionsExt;
    fs::write(path, contents)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    Ok(())
}

fn copy_schema(source: &Path, destination: &Path) -> TestResult {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let path = entry.path();
        let destination = destination.join(entry.file_name());
        if path.is_dir() {
            copy_schema(&path, &destination)?;
        } else if path.extension().is_some_and(|extension| extension == "cue") {
            fs::copy(path, destination)?;
        }
    }
    Ok(())
}

struct Lifecycle {
    directory: tempfile::TempDir,
    configuration: String,
    authentication_token: Option<String>,
    /// Every byte the CLI wrote to stdout and stderr, for leak assertions.
    transcript: Mutex<String>,
}

enum OutputFormat {
    Json,
    Text,
}

struct Invocation<'a> {
    arguments: &'a [&'a str],
    environment: &'a [(&'a str, &'a str)],
    format: OutputFormat,
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires a real Random provider and Turso/sqld; see module documentation"]
async fn moving_infrastructure_to_a_separate_package_preserves_state() -> TestResult {
    let provider = PathBuf::from(std::env::var("CUENV_INFRASTRUCTURE_TEST_RANDOM_PROVIDER")?)
        .canonicalize()?;
    let database = Database::start()?;
    let backend = TursoConfiguration {
        url: database.url().to_owned(),
        authentication_token: std::env::var("TURSO_AUTH_TOKEN").ok(),
    };
    let store = TursoStateStore::try_new(backend.clone())?;
    let project = format!("separate-infrastructure-{}", uuid::Uuid::new_v4());
    let tenant = TenantKey::try_new(MODULE, &project)?;
    let lifecycle = Lifecycle::new(&provider, &backend, &project)?;
    let created = lifecycle.run(&["apply", "--yes"])?;
    assert_eq!(created["applied"]["create"], 3);
    let before = store.list(&tenant).await?;
    assert_eq!(before.len(), 3);

    let (project_cue, infrastructure_cue) = lifecycle
        .configuration
        .split_once("infrastructure: {")
        .ok_or("missing infrastructure declaration")?;
    fs::write(lifecycle.directory.path().join("env.cue"), project_cue)?;
    fs::create_dir(lifecycle.directory.path().join("infra"))?;
    fs::write(
        lifecycle.directory.path().join("infra/stack.cue"),
        format!("package infrastructure\ninfrastructure: {{{infrastructure_cue}"),
    )?;
    fs::write(
        lifecycle.directory.path().join(".infrastructure.cue"),
        "package examples\ndirectory: \"infra\"\n\"package\": \"infrastructure\"\nprojectPackage: \"examples\"\n",
    )?;

    let plan = lifecycle.run(&["plan"])?;
    assert_eq!(plan["summary"]["create"], 0);
    assert_eq!(plan["summary"]["delete"], 0);
    assert_eq!(store.list(&tenant).await?.len(), before.len());
    lifecycle.run(&["destroy", "--yes"])?;
    assert!(store.list(&tenant).await?.is_empty());
    Ok(())
}

/// A temporary project directory holding a copy of the schema and a CUE module.
fn prepare_directory() -> TestResult<tempfile::TempDir> {
    let directory = tempfile::Builder::new()
        .prefix("cuenv-infrastructure-lifecycle-")
        .tempdir()?;
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    copy_schema(&repository.join("schema"), &directory.path().join("schema"))?;
    fs::create_dir_all(directory.path().join("cue.mod"))?;
    fs::write(
        directory.path().join("cue.mod/module.cue"),
        format!("module: \"{MODULE}\"\nlanguage: version: \"v0.14.1\"\n"),
    )?;
    Ok(directory)
}

impl Lifecycle {
    fn new(provider: &Path, backend: &TursoConfiguration, project: &str) -> TestResult<Self> {
        let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let example =
            fs::read_to_string(repository.join("examples/infrastructure-random/env.cue"))?;
        let configuration = example
            .replace(
                "\"infrastructure-random\"",
                &serde_json::to_string(project)?,
            )
            .replace(
                "state: turso: url: \"http://127.0.0.1:8080\"",
                &format!(
                    "state: turso: {{url: {}, token: {{cuenvPassthrough: true, name: \"TURSO_AUTH_TOKEN\"}}}}",
                    serde_json::to_string(&backend.url)?
                ),
            )
            .replace(
                "version: \"3.9.1\"",
                &format!("path: {}", serde_json::to_string(provider)?),
            );
        assert!(
            configuration.contains(PET_LENGTH),
            "example pet length changed"
        );
        assert!(!configuration.contains("version: \"3.9.1\""));
        Self::from_configuration(prepare_directory()?, configuration, backend)
    }

    fn from_configuration(
        directory: tempfile::TempDir,
        configuration: String,
        backend: &TursoConfiguration,
    ) -> TestResult<Self> {
        fs::write(directory.path().join("env.cue"), &configuration)?;
        Ok(Self {
            directory,
            configuration,
            authentication_token: backend.authentication_token.clone(),
            transcript: Mutex::new(String::new()),
        })
    }

    /// A project whose `dev` infrastructure environment has a provider that
    /// records the secret it was given, and an `env.environment.dev` exec
    /// secret released to that provider by `allowInfrastructure`.
    ///
    /// The variable uses the bare `{value, policies}` form the documentation
    /// teaches, so this test also proves that form reaches a provider.
    fn with_environment_secret(
        provider: &Path,
        backend: &TursoConfiguration,
        project: &str,
    ) -> TestResult<Self> {
        let directory = prepare_directory()?;
        let secret_command = directory.path().join("secret.sh");
        write_executable(
            &secret_command,
            &format!("#!/bin/sh\nprintf '%s' '{SECRET_VALUE}'\n"),
        )?;
        // The wrapper records what the provider process was given, then becomes
        // the real provider so the plugin handshake is unchanged.
        let wrapper = directory.path().join("provider-wrapper.sh");
        write_executable(
            &wrapper,
            &format!(
                "#!/bin/sh\nprintf '%s' \"${{{SECRET_VARIABLE}-unset}}\" > '{}'\nexec '{}' \"$@\"\n",
                directory.path().join(RECORDED_SECRET).display(),
                provider.display(),
            ),
        )?;
        let configuration = format!(
            r#"package examples

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: {project}

env: environment: dev: {SECRET_VARIABLE}: {{
	value: schema.#ExecSecret & {{command: {command}}}
	policies: [{{allowInfrastructure: ["plan", "apply", "destroy"]}}]
}}

infrastructure: {{
	state: turso: {{url: {url}, token: {{cuenvPassthrough: true, name: "TURSO_AUTH_TOKEN"}}}}
	environments: dev: {{
		providers: random: {{source: "hashicorp/random", path: {wrapper}}}
		resources: pet: {{
			type: "random_pet"
			configuration: {{length: 2, separator: "-"}}
		}}
	}}
}}
"#,
            project = serde_json::to_string(project)?,
            command = serde_json::to_string(&secret_command)?,
            url = serde_json::to_string(&backend.url)?,
            wrapper = serde_json::to_string(&wrapper)?,
        );
        Self::from_configuration(directory, configuration, backend)
    }

    /// A project whose remote cache reads its credentials from
    /// `CACHE_TOKEN_VARIABLE`, with a provider that records what it was given
    /// for that variable.
    fn with_remote_cache(
        provider: &Path,
        backend: &TursoConfiguration,
        project: &str,
    ) -> TestResult<Self> {
        let directory = prepare_directory()?;
        let wrapper = directory.path().join("provider-wrapper.sh");
        write_executable(
            &wrapper,
            &format!(
                "#!/bin/sh\nprintf '%s' \"${{{CACHE_TOKEN_VARIABLE}-unset}}\" > '{}'\nexec '{}' \"$@\"\n",
                directory.path().join(RECORDED_SECRET).display(),
                provider.display(),
            ),
        )?;
        let configuration = format!(
            r#"package examples

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: {project}

cache: remote: {{
	endpoint: "grpcs://cache.example.com"
	auth: bearerTokenEnv: "{CACHE_TOKEN_VARIABLE}"
}}

infrastructure: {{
	state: turso: {{url: {url}, token: {{cuenvPassthrough: true, name: "TURSO_AUTH_TOKEN"}}}}
	providers: random: {{source: "hashicorp/random", path: {wrapper}}}
	resources: pet: {{
		type: "random_pet"
		configuration: {{length: 2, separator: "-"}}
	}}
}}
"#,
            project = serde_json::to_string(project)?,
            url = serde_json::to_string(&backend.url)?,
            wrapper = serde_json::to_string(&wrapper)?,
        );
        Self::from_configuration(directory, configuration, backend)
    }

    fn recorded_secret(&self) -> TestResult<String> {
        Ok(fs::read_to_string(
            self.directory.path().join(RECORDED_SECRET),
        )?)
    }

    /// Run the CLI and record everything it printed.
    fn execute(&self, arguments: &[&str], environment: &[(&str, &str)]) -> TestResult<Output> {
        self.execute_invocation(&Invocation {
            arguments,
            environment,
            format: OutputFormat::Json,
        })
    }

    fn execute_text(&self, arguments: &[&str]) -> TestResult<Output> {
        self.execute_invocation(&Invocation {
            arguments,
            environment: &[],
            format: OutputFormat::Text,
        })
    }

    fn execute_invocation(&self, invocation: &Invocation<'_>) -> TestResult<Output> {
        let mut command = Command::new(env!("CARGO_BIN_EXE_cuenv"));
        command
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.directory.path().join(".runtime/home"))
            .env(
                "XDG_CACHE_HOME",
                self.directory.path().join(".runtime/cache"),
            )
            .env(
                "XDG_STATE_HOME",
                self.directory.path().join(".runtime/state"),
            )
            .env("XDG_DATA_HOME", self.directory.path().join(".runtime/data"))
            .current_dir(self.directory.path());
        if matches!(invocation.format, OutputFormat::Json) {
            command.arg("--json");
        }
        command
            .arg("infrastructure")
            .args(invocation.arguments)
            .args(["--package", "examples"]);
        if let Some(token) = &self.authentication_token {
            command.env("TURSO_AUTH_TOKEN", token);
        }
        command.envs(invocation.environment.iter().copied());
        // The CLI loads the platform's root certificates even for a plain-HTTP
        // state server; sandboxed builds provide them only through these variables.
        for variable in ["SSL_CERT_FILE", "SSL_CERT_DIR"] {
            if let Some(value) = std::env::var_os(variable) {
                command.env(variable, value);
            }
        }
        let output = command.output()?;
        {
            let mut transcript = self.transcript.lock().map_err(|_| "transcript poisoned")?;
            transcript.push_str(&String::from_utf8_lossy(&output.stdout));
            transcript.push_str(&String::from_utf8_lossy(&output.stderr));
        }
        Ok(output)
    }

    fn run(&self, arguments: &[&str]) -> TestResult<Value> {
        self.run_with_environment(arguments, &[])
    }

    /// Run the CLI with extra variables in its environment and expect success.
    fn run_with_environment(
        &self,
        arguments: &[&str],
        environment: &[(&str, &str)],
    ) -> TestResult<Value> {
        let output = self.execute(arguments, environment)?;
        assert!(
            output.status.success(),
            "cuenv infrastructure {} exited {}: {}{}",
            arguments.join(" "),
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        // Parsing the complete stdout also rejects extra JSON documents or logs.
        let envelope: Value = serde_json::from_slice(&output.stdout)?;
        assert_eq!(envelope["status"], "ok");
        Ok(envelope["data"].clone())
    }

    /// Run a command that must be refused as a configuration error (exit code
    /// 2) and return everything it printed.
    fn refused(&self, arguments: &[&str]) -> TestResult<String> {
        let output = self.execute(arguments, &[])?;
        let printed = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            output.status.code(),
            Some(2),
            "cuenv infrastructure {} was not refused: {printed}",
            arguments.join(" "),
        );
        Ok(printed)
    }

    fn transcript(&self) -> TestResult<String> {
        Ok(self
            .transcript
            .lock()
            .map_err(|_| "transcript poisoned")?
            .clone())
    }

    fn edit_pet(&self) -> TestResult {
        fs::write(
            self.directory.path().join("env.cue"),
            self.configuration.replace(PET_LENGTH, "length:    3"),
        )?;
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires a real Random provider and Turso/sqld; see module documentation"]
async fn random_provider_create_edit_destroy_stack() -> TestResult {
    let provider = PathBuf::from(std::env::var("CUENV_INFRASTRUCTURE_TEST_RANDOM_PROVIDER")?)
        .canonicalize()?;
    let database = Database::start()?;
    let backend = TursoConfiguration {
        url: database.url().to_owned(),
        authentication_token: std::env::var("TURSO_AUTH_TOKEN").ok(),
    };
    let store = TursoStateStore::try_new(backend.clone())?;
    let project = format!("random-lifecycle-{}", uuid::Uuid::new_v4());
    let tenant = TenantKey::try_new(MODULE, &project)?;
    let lifecycle = Lifecycle::new(&provider, &backend, &project)?;

    let plan = lifecycle.run(&["plan"])?;
    assert_eq!(plan["summary"]["create"], 3);
    assert!(
        store.list(&tenant).await?.is_empty(),
        "plan wrote resources"
    );
    assert!(
        store.last_writer(&tenant).await?.is_none(),
        "plan recorded a last writer"
    );

    let created = lifecycle.run(&["apply", "--yes"])?;
    assert_eq!(created["applied"]["create"], 3);
    assert_eq!(
        lifecycle.run(&["state", "list"])?["resources"]
            .as_array()
            .ok_or("no resources")?
            .len(),
        3
    );
    let initial = store.list(&tenant).await?;
    assert_eq!(initial.len(), 3);
    assert!(
        initial
            .iter()
            .all(|resource| resource.provider_source == "registry.terraform.io/hashicorp/random")
    );
    let pet = initial
        .iter()
        .find(|resource| resource.address.to_string() == "random_pet.pet")
        .ok_or("pet was not recorded")?;
    let identifier = pet.state["id"].as_str().ok_or("pet has no identifier")?;
    assert_eq!(pet.state["length"], 2);
    assert_eq!(identifier.split('-').count(), 2);
    assert!(store.current_lock(&tenant).await?.is_none());

    let unchanged = lifecycle.run(&["apply", "--yes"])?;
    assert!(unchanged["applied"].is_null(), "unchanged stack had work");
    assert_eq!(unchanged["summary"]["unchanged"], 3);
    assert!(
        store.list(&tenant).await? == initial,
        "no-op apply changed state"
    );

    // Edit the actual CUE input: Random requires replacing a pet when length changes.
    lifecycle.edit_pet()?;
    let edit = lifecycle.run(&["plan"])?;
    assert_eq!(edit["summary"]["replace"], 1);
    assert_eq!(edit["summary"]["unchanged"], 2);
    assert!(
        store.list(&tenant).await? == initial,
        "plan changed stored state"
    );
    let edited = lifecycle.run(&["apply", "--yes"])?;
    assert_eq!(edited["applied"]["replace"], 1);
    let updated = store.list(&tenant).await?;
    assert_eq!(updated.len(), 3);
    let pet = updated
        .iter()
        .find(|resource| resource.address.to_string() == "random_pet.pet")
        .ok_or("edited pet was not recorded")?;
    let updated_identifier = pet.state["id"]
        .as_str()
        .ok_or("edited pet has no identifier")?;
    assert_eq!(pet.state["length"], 3);
    assert_eq!(updated_identifier.split('-').count(), 3);
    assert_ne!(updated_identifier, identifier);
    for original in initial
        .iter()
        .filter(|resource| resource.address.name != "pet")
    {
        assert!(
            updated.iter().any(|resource| resource == original),
            "pet replacement changed {}",
            original.address,
        );
    }
    assert!(store.current_lock(&tenant).await?.is_none());

    let destroyed = lifecycle.run(&["destroy", "--yes"])?;
    assert_eq!(destroyed["applied"]["delete"], 3);
    assert_eq!(
        lifecycle.run(&["state", "list"])?["resources"],
        serde_json::json!([])
    );
    assert!(store.list(&tenant).await?.is_empty());
    assert!(store.current_lock(&tenant).await?.is_none());
    let destroyed_again = lifecycle.run(&["destroy", "--yes"])?;
    assert!(destroyed_again["applied"].is_null());
    Ok(())
}

/// `--env dev` with an `env.environment.dev` exec secret consumed by the
/// provider: the secret reaches the provider process, is never printed or
/// stored, and the named environment applies and destroys independently.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires a real Random provider and Turso/sqld; see module documentation"]
async fn named_environment_passes_an_exec_secret_to_the_provider() -> TestResult {
    let provider = PathBuf::from(std::env::var("CUENV_INFRASTRUCTURE_TEST_RANDOM_PROVIDER")?)
        .canonicalize()?;
    let database = Database::start()?;
    let backend = TursoConfiguration {
        url: database.url().to_owned(),
        authentication_token: std::env::var("TURSO_AUTH_TOKEN").ok(),
    };
    let store = TursoStateStore::try_new(backend.clone())?;
    // The shared database may be fresh, or another test may be migrating it
    // through version 1 right now: read only once it is current.
    store.migrate().await?;
    let project = format!("random-secret-lifecycle-{}", uuid::Uuid::new_v4());
    let tenant = TenantKey::with_environment(MODULE, &project, "dev")?;
    // A fresh project name means the no-flag namespace holds no rows to migrate.
    let legacy = TenantKey::try_new(MODULE, &project)?;
    assert!(store.list(&legacy).await?.is_empty());
    let lifecycle = Lifecycle::with_environment_secret(&provider, &backend, &project)?;

    let plan = lifecycle.run(&["plan", "--env", "dev"])?;
    assert_eq!(plan["summary"]["create"], 1);
    assert!(
        store.list(&tenant).await?.is_empty(),
        "plan wrote resources"
    );

    let created = lifecycle.run(&["apply", "--env", "dev", "--yes"])?;
    assert_eq!(created["applied"]["create"], 1);
    assert_eq!(
        lifecycle.recorded_secret()?,
        SECRET_VALUE,
        "the provider did not receive the authorised secret"
    );
    let recorded = store.list(&tenant).await?;
    assert_eq!(recorded.len(), 1);
    assert!(
        store.list(&legacy).await?.is_empty(),
        "the named environment wrote to the no-flag namespace"
    );
    assert!(
        !serde_json::to_string(&recorded.iter().map(|row| &row.state).collect::<Vec<_>>())?
            .contains(SECRET_VALUE),
        "the secret was stored in state"
    );
    assert!(store.current_lock(&tenant).await?.is_none());

    let destroyed = lifecycle.run(&["destroy", "--env", "dev", "--yes"])?;
    assert_eq!(destroyed["applied"]["delete"], 1);
    assert!(store.list(&tenant).await?.is_empty());
    assert!(store.current_lock(&tenant).await?.is_none());

    let transcript = lifecycle.transcript()?;
    assert!(
        !transcript.is_empty(),
        "the transcript recorded no CLI output"
    );
    assert!(
        !transcript.contains(SECRET_VALUE),
        "the secret appeared in CLI output"
    );
    Ok(())
}

/// Where `random_pet.pet` is declared.
#[derive(Debug, Clone, Copy)]
enum PetDeclaration {
    /// In the `dev` environment only.
    InEnvironment,
    /// At the top level, while `dev` declares another resource.
    AtTopLevel,
}

/// One project of the identity conflict test.
struct ConflictProject<'project> {
    provider: &'project Path,
    backend: &'project TursoConfiguration,
    name: &'project str,
}

impl ConflictProject<'_> {
    /// The configuration at the two stages of moving a resource from an
    /// environment to the top level.
    fn configuration(&self, pet: PetDeclaration) -> TestResult<String> {
        let provider_block = format!(
            "providers: random: {{source: \"hashicorp/random\", path: {}}}",
            serde_json::to_string(self.provider)?
        );
        let pet_resource =
            "pet: {type: \"random_pet\", configuration: {length: 2, separator: \"-\"}}";
        let (top_level, environment_resource) = match pet {
            PetDeclaration::AtTopLevel => (
                format!("{provider_block}\n\tresources: {pet_resource}"),
                "identifier: {type: \"random_id\", configuration: byte_length: 4}",
            ),
            PetDeclaration::InEnvironment => (String::new(), pet_resource),
        };
        Ok(format!(
            r#"package examples

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: {project}

infrastructure: {{
	state: turso: {{url: {url}, token: {{cuenvPassthrough: true, name: "TURSO_AUTH_TOKEN"}}}}
	{top_level}
	environments: dev: {{
		{provider_block}
		resources: {environment_resource}
	}}
}}
"#,
            project = serde_json::to_string(self.name)?,
            url = serde_json::to_string(&self.backend.url)?,
        ))
    }
}

/// A run without `--env` must not create an address a declared environment
/// already records: both identities would manage one real object, and a later
/// `destroy --env dev` would delete what the top-level configuration manages.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires a real Random provider and Turso/sqld; see module documentation"]
async fn a_run_without_env_cannot_claim_what_an_environment_records() -> TestResult {
    let provider = PathBuf::from(std::env::var("CUENV_INFRASTRUCTURE_TEST_RANDOM_PROVIDER")?)
        .canonicalize()?;
    let database = Database::start()?;
    let backend = TursoConfiguration {
        url: database.url().to_owned(),
        authentication_token: std::env::var("TURSO_AUTH_TOKEN").ok(),
    };
    let store = TursoStateStore::try_new(backend.clone())?;
    let project = format!("random-identity-{}", uuid::Uuid::new_v4());
    let unselected = TenantKey::try_new(MODULE, &project)?;
    let dev = TenantKey::with_environment(MODULE, &project, "dev")?;
    let conflict = ConflictProject {
        provider: &provider,
        backend: &backend,
        name: &project,
    };
    let lifecycle = Lifecycle::from_configuration(
        prepare_directory()?,
        conflict.configuration(PetDeclaration::InEnvironment)?,
        &backend,
    )?;
    let created = lifecycle.run(&["apply", "--env", "dev", "--yes"])?;
    assert_eq!(created["applied"]["create"], 1);

    // The pet moves to the top level; `dev` keeps declaring something else.
    fs::write(
        lifecycle.directory.path().join("env.cue"),
        conflict.configuration(PetDeclaration::AtTopLevel)?,
    )?;
    // A plan only warns.
    lifecycle.run(&["plan"])?;
    let printed = lifecycle.refused(&["apply", "--yes"])?;
    assert!(printed.contains("random_pet.pet"), "{printed}");
    assert!(printed.contains("'dev'"), "{printed}");
    assert!(printed.contains("--allow-separate-state"), "{printed}");
    assert!(
        store.list(&unselected).await?.is_empty(),
        "the refused run recorded resources"
    );
    assert_eq!(store.list(&dev).await?.len(), 1);

    // The override creates separate objects, and each identity can then be
    // destroyed on its own.
    let separate = lifecycle.run(&["apply", "--yes", "--allow-separate-state"])?;
    assert_eq!(separate["applied"]["create"], 1);
    assert_eq!(store.list(&unselected).await?.len(), 1);
    lifecycle.run(&["destroy", "--yes"])?;
    lifecycle.run(&["destroy", "--env", "dev", "--yes"])?;
    assert!(store.list(&unselected).await?.is_empty());
    assert!(store.list(&dev).await?.is_empty());
    Ok(())
}

/// The variable the project's `cache.remote.auth` names holds the remote
/// cache's credential: a provider (which inherits the rest of the environment)
/// must not receive it, and no output may contain it.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires a real Random provider and Turso/sqld; see module documentation"]
async fn the_remote_cache_credential_never_reaches_a_provider() -> TestResult {
    let provider = PathBuf::from(std::env::var("CUENV_INFRASTRUCTURE_TEST_RANDOM_PROVIDER")?)
        .canonicalize()?;
    let database = Database::start()?;
    let backend = TursoConfiguration {
        url: database.url().to_owned(),
        authentication_token: std::env::var("TURSO_AUTH_TOKEN").ok(),
    };
    let project = format!("random-cache-token-{}", uuid::Uuid::new_v4());
    let lifecycle = Lifecycle::with_remote_cache(&provider, &backend, &project)?;
    let environment = [(CACHE_TOKEN_VARIABLE, CACHE_TOKEN_VALUE)];

    let created = lifecycle.run_with_environment(&["apply", "--yes"], &environment)?;
    assert_eq!(created["applied"]["create"], 1);
    assert_eq!(
        lifecycle.recorded_secret()?,
        "unset",
        "the provider received the remote cache credential"
    );
    lifecycle.run_with_environment(&["destroy", "--yes"], &environment)?;
    assert!(
        !lifecycle.transcript()?.contains(CACHE_TOKEN_VALUE),
        "the remote cache credential appeared in CLI output"
    );
    Ok(())
}

/// A project moved to another directory keeps its state: the key has no
/// directory, and the last writer is information only.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires a real Random provider and Turso/sqld; see module documentation"]
async fn a_moved_project_keeps_its_state() -> TestResult {
    let provider = PathBuf::from(std::env::var("CUENV_INFRASTRUCTURE_TEST_RANDOM_PROVIDER")?)
        .canonicalize()?;
    let database = Database::start()?;
    let backend = TursoConfiguration {
        url: database.url().to_owned(),
        authentication_token: std::env::var("TURSO_AUTH_TOKEN").ok(),
    };
    let project = format!("random-owner-{}", uuid::Uuid::new_v4());
    let directory = prepare_directory()?;
    let marker = directory.path().join("secret-ran");
    let secret = directory.path().join("secret.sh");
    write_executable(
        &secret,
        &format!(
            "#!/bin/sh\ntouch '{}'\nprintf '%s' value\n",
            marker.display()
        ),
    )?;
    let configuration = format!(
        r#"package examples

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: {project}

env: SOME_TOKEN: schema.#ExecSecret & {{command: {secret}}}

infrastructure: {{
	state: turso: {{url: {url}, token: {{cuenvPassthrough: true, name: "TURSO_AUTH_TOKEN"}}}}
	providers: random: {{source: "hashicorp/random", path: {provider}}}
	resources: pet: {{type: "random_pet", configuration: {{length: 2, separator: "-"}}}}
}}
"#,
        project = serde_json::to_string(&project)?,
        secret = serde_json::to_string(&secret)?,
        url = serde_json::to_string(&backend.url)?,
        provider = serde_json::to_string(&provider)?,
    );
    let root = directory.path().to_path_buf();
    let lifecycle = Lifecycle::from_configuration(directory, configuration.clone(), &backend)?;

    lifecycle.run(&["apply", "--yes"])?;
    assert!(marker.exists(), "the project's secret did not run");

    // The project moves to another directory under the same name: it keeps
    // its state, plans no change, and becomes the last writer.
    fs::create_dir_all(root.join("moved"))?;
    fs::write(root.join("moved/env.cue"), &configuration)?;
    fs::remove_file(root.join("env.cue"))?;
    let moved = lifecycle.run(&["apply", "--yes", "-p", "moved"])?;
    assert!(moved["applied"].is_null(), "{moved}");
    let store = TursoStateStore::try_new(backend.clone())?;
    let tenant = TenantKey::try_new(MODULE, &project)?;
    assert_eq!(
        store
            .last_writer(&tenant)
            .await?
            .ok_or("no last writer")?
            .instance
            .as_str(),
        "moved:examples"
    );
    lifecycle.run(&["destroy", "--yes", "-p", "moved"])?;
    Ok(())
}

/// Init restores locked types from a verified local provider cache and refuses
/// schema digest drift without evaluating the project or modifying the lock.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the fake provider; see module documentation"]
async fn infrastructure_init_restores_locked_types_from_cached_provider() -> TestResult {
    use cuenv_infrastructure::Cancellation;
    use cuenv_infrastructure::cue_types::{self, ProviderRelease};
    use cuenv_infrastructure::registry::{ProviderInstaller, ProviderSource, current_platform};
    use sha2::{Digest, Sha256};

    let provider =
        PathBuf::from(std::env::var("CUENV_INFRASTRUCTURE_TEST_FAKE_PROVIDER")?).canonicalize()?;
    let directory = prepare_directory()?;
    // An unresolvable project ensures init never attempts project evaluation.
    fs::write(
        directory.path().join("env.cue"),
        "package cuenv\nimport missing \"example.invalid/test/fake/resources/fake_plain\"\nname: missing.name\n",
    )?;
    let source = ProviderSource::parse("example.invalid/test/fake")?;
    let version = "1.2.3";
    let (schema, _) = cue_types::load_schema(&provider, &Cancellation::default()).await?;
    let expected = cue_types::render(
        &schema,
        ProviderRelease {
            source: &source,
            version,
        },
    )?;
    let cache = directory.path().join("provider-cache");
    let installation =
        ProviderInstaller::try_new(cache.clone())?.install_directory(&source, version)?;
    fs::create_dir_all(&installation)?;
    let binary = installation.join("terraform-provider-fake");
    fs::copy(&provider, &binary)?;
    let binary_digest = hex::encode(Sha256::digest(fs::read(&binary)?));
    let archive_digest = "a".repeat(64);
    fs::write(
        installation.join(".cuenv-provider.json"),
        serde_json::to_vec(&serde_json::json!({
            "filename": "terraform-provider-fake",
            "sha256": binary_digest,
            "archive_sha256": archive_digest,
            "source": source.to_string(),
            "version": version,
        }))?,
    )?;
    let platform = current_platform()?.to_string();
    let lockfile = |schema_digest: &str| {
        format!(
            "version = 5\n[infrastructure_providers.\"{source}\"]\nversion = \"{version}\"\nschema_digest = \"{schema_digest}\"\n[infrastructure_providers.\"{source}\".platforms]\n{platform} = \"sha256:{archive_digest}\"\n"
        )
    };
    let lock = lockfile(&expected.schema_digest);
    fs::write(directory.path().join("cuenv.lock"), &lock)?;
    let init = || -> Result<Output, std::io::Error> {
        Command::new(env!("CARGO_BIN_EXE_cuenv"))
            .current_dir(directory.path())
            .env("TF_PLUGIN_CACHE_DIR", &cache)
            .args(["infra", "init"])
            .output()
    };

    let restored = init()?;
    assert!(
        restored.status.success(),
        "{}",
        String::from_utf8_lossy(&restored.stderr)
    );
    assert_eq!(
        fs::read_to_string(directory.path().join("cuenv.lock"))?,
        lock
    );
    for (relative, content) in &expected.files {
        assert_eq!(
            fs::read_to_string(directory.path().join(&expected.directory).join(relative))?,
            *content
        );
    }
    let ignore = fs::read_to_string(directory.path().join(".gitignore"))?;
    let repeated = init()?;
    assert!(
        repeated.status.success(),
        "{}",
        String::from_utf8_lossy(&repeated.stderr)
    );
    assert!(String::from_utf8_lossy(&repeated.stdout).contains("up to date"));
    assert_eq!(
        fs::read_to_string(directory.path().join(".gitignore"))?,
        ignore
    );
    assert_eq!(
        fs::read_to_string(directory.path().join("cuenv.lock"))?,
        lock
    );

    // Digest drift must be refused without rewriting generated types or the pin.
    let stale_lock = lockfile(&format!("sha256:{}", "b".repeat(64)));
    fs::write(directory.path().join("cuenv.lock"), &stale_lock)?;
    let stale = init()?;
    assert!(!stale.status.success());
    assert!(String::from_utf8_lossy(&stale.stderr).contains("schema digest differs"));
    assert_eq!(
        fs::read_to_string(directory.path().join("cuenv.lock"))?,
        stale_lock
    );
    for (relative, content) in &expected.files {
        assert_eq!(
            fs::read_to_string(directory.path().join(&expected.directory).join(relative))?,
            *content
        );
    }
    Ok(())
}

/// Typed configuration end to end: CUE types generated from the provider
/// binary into the module's `cue.mod/gen` type the project and a typed plan
/// runs; a misspelled argument fails evaluation before any provider starts,
/// and a `schemaDigest` that is not the provider's is refused.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires a real Random provider and Turso/sqld; see module documentation"]
async fn generated_provider_types_type_the_project() -> TestResult {
    use cuenv_infrastructure::Cancellation;
    use cuenv_infrastructure::cue_types::{self, ProviderRelease};
    use cuenv_infrastructure::registry::ProviderSource;

    let provider = PathBuf::from(std::env::var("CUENV_INFRASTRUCTURE_TEST_RANDOM_PROVIDER")?)
        .canonicalize()?;
    let database = Database::start()?;
    let backend = TursoConfiguration {
        url: database.url().to_owned(),
        authentication_token: std::env::var("TURSO_AUTH_TOKEN").ok(),
    };
    let directory = prepare_directory()?;
    let (schema, _) = cue_types::load_schema(&provider, &Cancellation::default()).await?;
    let source = ProviderSource::parse("hashicorp/random")?;
    let types = cue_types::render(
        &schema,
        ProviderRelease {
            source: &source,
            version: "0.0.0",
        },
    )?;
    for (relative, content) in &types.files {
        let path = directory.path().join(&types.directory).join(relative);
        fs::create_dir_all(path.parent().ok_or("generated file has no parent")?)?;
        fs::write(path, content)?;
    }

    // A local provider `path` keeps the run offline, so the provider block
    // is typed with `#Configuration` rather than declared by `#Provider`
    // (which pins a registry version).
    let configuration = |pet: &str, digest: &str| -> TestResult<String> {
        Ok(format!(
            r#"package examples

import (
	"github.com/cuenv/cuenv/schema"
	"registry.terraform.io/hashicorp/random:random_provider"
	"registry.terraform.io/hashicorp/random/resources/random_password"
	"registry.terraform.io/hashicorp/random/resources/random_pet"
)

schema.#Project

name: {project}

infrastructure: {{
	state: turso: {{url: {url}, token: {{cuenvPassthrough: true, name: "TURSO_AUTH_TOKEN"}}}}
	providers: random: {{
		source:        "hashicorp/random"
		path:          {provider}
		schemaDigest:  {digest}
		configuration: random_provider.#Configuration
	}}
	resources: {{
		pet: random_pet.#Resource & {{configuration: {{{pet}, separator: "-"}}}}
		consumer: random_pet.#Resource & {{configuration: {{length: 1, prefix: infrastructure.resources.pet.outputs.id}}}}
		password: random_password.#Resource & {{
			dependsOn: ["pet"]
			configuration: {{length: 16, special: false}}
		}}
	}}
}}
"#,
            project = serde_json::to_string(&format!("typed-{}", uuid::Uuid::new_v4()))?,
            url = serde_json::to_string(&backend.url)?,
            provider = serde_json::to_string(&provider)?,
            digest = serde_json::to_string(digest)?,
        ))
    };

    let lifecycle = Lifecycle::from_configuration(
        directory,
        configuration("length: 2", &types.schema_digest)?,
        &backend,
    )?;
    let plan = lifecycle.run(&["plan"])?;
    assert_eq!(plan["summary"]["create"], 3);

    let env_cue = lifecycle.directory.path().join("env.cue");
    fs::write(&env_cue, configuration("lenght: 2", &types.schema_digest)?)?;
    let output = lifecycle.execute(&["plan"], &[])?;
    let printed = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.status.code(), Some(3), "{printed}");
    assert!(
        printed.contains("CUE infrastructure configuration could not be evaluated"),
        "{printed}"
    );
    assert!(!printed.contains("lenght"), "{printed}");

    let stale = format!("sha256:{}", "0".repeat(64));
    fs::write(&env_cue, configuration("length: 2", &stale)?)?;
    let refused = lifecycle.refused(&["plan"])?;
    assert!(refused.contains("were generated from schema"), "{refused}");
    Ok(())
}

/// Native selectors resolve to provider inputs, infer dependencies and produce
/// a subsequent no-op plan. An untyped source declaration that promises the
/// wrong primitive type is rejected against the live provider schema.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires a real Random provider and Turso/sqld; see module documentation"]
async fn native_output_selectors_resolve_and_infer_dependencies() -> TestResult {
    let provider = PathBuf::from(std::env::var("CUENV_INFRASTRUCTURE_TEST_RANDOM_PROVIDER")?)
        .canonicalize()?;
    let database = Database::start()?;
    let backend = TursoConfiguration {
        url: database.url().to_owned(),
        authentication_token: std::env::var("TURSO_AUTH_TOKEN").ok(),
    };
    let project = format!("native-output-{}", uuid::Uuid::new_v4());
    let tenant = TenantKey::try_new(MODULE, &project)?;
    let store = TursoStateStore::try_new(backend.clone())?;
    let configuration = format!(
        r#"package examples
import "github.com/cuenv/cuenv/schema"
schema.#Project
name: {project}
infrastructure: {{
 state: turso: {{url: {url}, token: {{cuenvPassthrough: true, name: "TURSO_AUTH_TOKEN"}}}}
 providers: random: {{source: "hashicorp/random", path: {provider}}}
 resources: {{
  primary: {{type: "random_pet", outputs: id: string, configuration: {{length: 1}}}}
  consumer: {{type: "random_pet", configuration: {{length: 1, prefix: infrastructure.resources.primary.outputs.id}}}}
 }}
}}
"#,
        project = serde_json::to_string(&project)?,
        url = serde_json::to_string(&backend.url)?,
        provider = serde_json::to_string(&provider)?,
    );
    let lifecycle =
        Lifecycle::from_configuration(prepare_directory()?, configuration.clone(), &backend)?;
    let initial = lifecycle.run(&["plan"])?;
    assert_eq!(initial["summary"]["create"], 2);
    let created = lifecycle.run(&["apply", "--yes"])?;
    assert_eq!(created["applied"]["create"], 2);
    let rows = store.list(&tenant).await?;
    let primary = rows
        .iter()
        .find(|row| row.address.name == "primary")
        .ok_or("primary missing")?;
    let consumer = rows
        .iter()
        .find(|row| row.address.name == "consumer")
        .ok_or("consumer missing")?;
    assert_eq!(consumer.state["prefix"], primary.state["id"]);
    assert_eq!(consumer.dependencies, ["random_pet.primary"]);
    assert_eq!(lifecycle.run(&["plan"])?["summary"]["unchanged"], 2);

    fs::write(
        lifecycle.directory.path().join("env.cue"),
        configuration.replace("outputs: id: string", "outputs: id: number"),
    )?;
    let refused = lifecycle.refused(&["plan"])?;
    assert!(
        refused.contains("native CUE output declaration type does not match the provider schema"),
        "{refused}"
    );
    assert_eq!(store.list(&tenant).await?, rows);
    fs::write(lifecycle.directory.path().join("env.cue"), configuration)?;
    lifecycle.run(&["destroy", "--yes"])?;
    Ok(())
}

/// Native selectors reach generated nested sensitive attributes through the CLI.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the fake provider and Turso/sqld; see module documentation"]
async fn generated_nested_token_references_plan_and_apply_through_cli() -> TestResult {
    use cuenv_infrastructure::Cancellation;
    use cuenv_infrastructure::cue_types::{self, ProviderRelease};
    use cuenv_infrastructure::registry::ProviderSource;

    let provider =
        PathBuf::from(std::env::var("CUENV_INFRASTRUCTURE_TEST_FAKE_PROVIDER")?).canonicalize()?;
    let database = Database::start()?;
    let backend = TursoConfiguration {
        url: database.url().to_owned(),
        authentication_token: std::env::var("TURSO_AUTH_TOKEN").ok(),
    };
    let directory = prepare_directory()?;
    let (schema, _) = cue_types::load_schema(&provider, &Cancellation::default()).await?;
    let source = ProviderSource::parse("example.com/test/fake")?;
    let types = cue_types::render(
        &schema,
        ProviderRelease {
            source: &source,
            version: "0.0.0",
        },
    )?;
    for (relative, content) in &types.files {
        let path = directory.path().join(&types.directory).join(relative);
        fs::create_dir_all(path.parent().ok_or("generated file has no parent")?)?;
        fs::write(path, content)?;
    }
    let project = format!("nested-token-lifecycle-{}", uuid::Uuid::new_v4());
    let tenant = TenantKey::try_new(MODULE, &project)?;
    let store = TursoStateStore::try_new(backend.clone())?;
    let configuration = format!(
        r#"package examples
import (
 "github.com/cuenv/cuenv/schema"
 "example.com/test/fake:fake_provider"
 "example.com/test/fake/resources/fake_token_source"
 "example.com/test/fake/resources/fake_token_consumer"
)
schema.#Project
name: {project}
infrastructure: {{
 state: turso: {{url: {url}, token: {{cuenvPassthrough: true, name: "TURSO_AUTH_TOKEN"}}}}
 providers: fake: {{
  source: "example.com/test/fake"
  path: {provider}
  schemaDigest: {digest}
  configuration: fake_provider.#Configuration & {{directory: {directory}}}
 }}
 resources: {{
  producer: fake_token_source.#Resource & {{configuration: name: "first"}}
  native: fake_token_consumer.#Resource & {{configuration: config: {{
   label: "native"
   token: infrastructure.resources.producer.outputs.token
  }}}}
  second_native: fake_token_consumer.#Resource & {{configuration: config: {{
   label: "second"
   token: infrastructure.resources.producer.outputs.token
  }}}}
 }}
}}
"#,
        project = serde_json::to_string(&project)?,
        url = serde_json::to_string(&backend.url)?,
        provider = serde_json::to_string(&provider)?,
        digest = serde_json::to_string(&types.schema_digest)?,
        directory = serde_json::to_string(directory.path())?,
    );
    let lifecycle = Lifecycle::from_configuration(directory, configuration, &backend)?;
    let preview = lifecycle.execute_text(&["plan"])?;
    assert!(
        preview.status.success(),
        "{}",
        String::from_utf8_lossy(&preview.stderr)
    );
    let preview_text = String::from_utf8_lossy(&preview.stdout);
    assert!(
        preview_text.contains("config.token = (sensitive)"),
        "{preview_text}"
    );
    assert_eq!(lifecycle.run(&["apply", "--yes"])?["applied"]["create"], 3);
    let rows = store.list(&tenant).await?;
    let producer = rows
        .iter()
        .find(|row| row.address.name == "producer")
        .ok_or("producer missing")?;
    let token = producer.state["token"].as_str().ok_or("token missing")?;
    for name in ["native", "second_native"] {
        let consumer = rows
            .iter()
            .find(|row| row.address.name == name)
            .ok_or("consumer missing")?;
        assert_eq!(consumer.state["config"]["token"], token);
        assert_eq!(consumer.dependencies, ["fake_token_source.producer"]);
        assert_eq!(consumer.sensitive_attributes, ["config"]);
    }
    assert!(!lifecycle.transcript()?.contains(token));
    let journal = fs::read_to_string(lifecycle.directory.path().join("journal.log"))?;
    let source_create = journal
        .find("token_source: Create")
        .ok_or("producer was not created")?;
    let sink_create = journal
        .find("token_consumer: Create")
        .ok_or("consumer was not created")?;
    assert!(source_create < sink_create);
    assert_eq!(lifecycle.run(&["plan"])?["summary"]["unchanged"], 3);
    lifecycle.run(&["destroy", "--yes"])?;
    Ok(())
}

/// Generated data source packages type actual CUE inputs, and native selectors
/// resolve fresh lookup results through the CLI.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the fake provider and Turso/sqld; see module documentation"]
async fn generated_data_sources_resolve_native_selectors() -> TestResult {
    use cuenv_infrastructure::Cancellation;
    use cuenv_infrastructure::cue_types::{self, ProviderRelease};
    use cuenv_infrastructure::registry::ProviderSource;

    let provider =
        PathBuf::from(std::env::var("CUENV_INFRASTRUCTURE_TEST_FAKE_PROVIDER")?).canonicalize()?;
    let database = Database::start()?;
    let backend = TursoConfiguration {
        url: database.url().to_owned(),
        authentication_token: std::env::var("TURSO_AUTH_TOKEN").ok(),
    };
    let directory = prepare_directory()?;
    let (schema, _) = cue_types::load_schema(&provider, &Cancellation::default()).await?;
    let source = ProviderSource::parse("example.com/test/fake")?;
    let types = cue_types::render(
        &schema,
        ProviderRelease {
            source: &source,
            version: "0.0.0",
        },
    )?;
    for (relative, content) in &types.files {
        let path = directory.path().join(&types.directory).join(relative);
        fs::create_dir_all(path.parent().ok_or("generated file has no parent")?)?;
        fs::write(path, content)?;
    }
    let wrapper = directory.path().join("provider-wrapper.sh");
    let launches = directory.path().join("provider-launches.log");
    let shell_quote = |path: &Path| format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"));
    write_executable(
        &wrapper,
        &format!(
            "#!/bin/sh\nprintf '%s\\n' started >> {}\nexec {} \"$@\"\n",
            shell_quote(&launches),
            shell_quote(&provider),
        ),
    )?;
    let project = format!("data-source-lifecycle-{}", uuid::Uuid::new_v4());
    let tenant = TenantKey::try_new(MODULE, &project)?;
    let store = TursoStateStore::try_new(backend.clone())?;
    let configuration = format!(
        r#"package examples
import (
 "github.com/cuenv/cuenv/schema"
 "example.com/test/fake:fake_provider"
 "example.com/test/fake/data/fake_lookup"
 "example.com/test/fake/resources/fake_plain"
)
schema.#Project
name: {project}
infrastructure: {{
 state: turso: {{url: {url}, token: {{cuenvPassthrough: true, name: "TURSO_AUTH_TOKEN"}}}}
 providers: fake: {{
  source: "example.com/test/fake"
  path: {provider}
  schemaDigest: {digest}
  configuration: fake_provider.#Configuration & {{directory: {directory}}}
 }}
 data: lookup: fake_lookup.#DataSource & {{configuration: {{name: "outside"}}}}
 resources: {{
  native: fake_plain.#Resource & {{configuration: {{name: infrastructure.data.lookup.outputs.value}}}}
  second_native: fake_plain.#Resource & {{configuration: {{name: infrastructure.data.lookup.outputs.value}}}}
  secret: fake_plain.#Resource & {{configuration: {{name: infrastructure.data.lookup.outputs.secret_value}}}}
 }}
}}
"#,
        project = serde_json::to_string(&project)?,
        url = serde_json::to_string(&backend.url)?,
        provider = serde_json::to_string(&wrapper)?,
        digest = serde_json::to_string(&types.schema_digest)?,
        directory = serde_json::to_string(directory.path())?,
    );
    let lifecycle = Lifecycle::from_configuration(directory, configuration.clone(), &backend)?;
    assert_eq!(lifecycle.run(&["plan"])?["summary"]["create"], 3);
    assert!(store.list(&tenant).await?.is_empty());
    assert_eq!(lifecycle.run(&["apply", "--yes"])?["applied"]["create"], 3);
    let rows = store.list(&tenant).await?;
    assert_eq!(rows.len(), 3, "data sources must not create state rows");
    for name in ["native", "second_native"] {
        let consumer = rows
            .iter()
            .find(|row| row.address.name == name)
            .ok_or("consumer missing")?;
        assert_eq!(consumer.state["name"], "lookup:outside");
    }
    let secret = rows
        .iter()
        .find(|row| row.address.name == "secret")
        .ok_or("secret consumer missing")?
        .state["name"]
        .as_str()
        .ok_or("missing secret value")?;
    assert!(secret.starts_with("lookup-secret-"));
    assert!(!lifecycle.transcript()?.contains(secret));

    // A separate apply invocation must read the remote source again, even
    // though no configuration changed and no data state was saved.
    fs::write(
        lifecycle.directory.path().join("lookup-value"),
        "new-remote-value",
    )?;
    assert_eq!(lifecycle.run(&["apply", "--yes"])?["applied"]["update"], 2);
    let updated = store.list(&tenant).await?;
    assert_eq!(updated.len(), 3);
    for name in ["native", "second_native"] {
        let consumer = updated
            .iter()
            .find(|row| row.address.name == name)
            .ok_or("updated consumer missing")?;
        assert_eq!(consumer.state["name"], "new-remote-value");
    }
    assert_eq!(lifecycle.run(&["plan"])?["summary"]["unchanged"], 3);

    let env_cue = lifecycle.directory.path().join("env.cue");
    let journal = lifecycle.directory.path().join("journal.log");
    let before = fs::read_to_string(&journal)?;
    let selector = "name: infrastructure.data.lookup.outputs.value";
    let typed_consumer = "native: fake_plain.#Resource & {configuration: {name: infrastructure.data.lookup.outputs.value}}";
    assert!(configuration.contains(typed_consumer));
    for descriptor in [
        r#"{fromResource: "fake_plain.second_native", attribute: "name"}"#,
        r#"{fromDataSource: "fake_lookup.lookup", attribute: "value"}"#,
    ] {
        let launches_before = fs::read_to_string(&launches)?;
        // Generated types reject the old object at the string field before
        // starting the provider, even when the descriptor is otherwise valid.
        fs::write(
            &env_cue,
            configuration.replacen(selector, &format!("name: {descriptor}"), 1),
        )?;
        let output = lifecycle.execute(&["plan"], &[])?;
        let printed = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.status.code(), Some(3), "{printed}");
        assert!(
            printed.contains("CUE infrastructure configuration could not be evaluated"),
            "{printed}"
        );
        assert_eq!(fs::read_to_string(&launches)?, launches_before);
        assert_eq!(fs::read_to_string(&journal)?, before);
        assert_eq!(store.list(&tenant).await?, updated);

        // Untyped fields are checked against the provider schema after the
        // provider starts. No resource or data-source operation may run.
        let untyped_consumer =
            format!("native: {{type: \"fake_plain\", configuration: {{name: {descriptor}}}}}");
        fs::write(
            &env_cue,
            configuration.replacen(typed_consumer, &untyped_consumer, 1),
        )?;
        let output = lifecycle.execute(&["plan"], &[])?;
        let printed = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!output.status.success(), "{printed}");
        assert!(
            printed.contains("name") && printed.contains("expected string"),
            "{printed}"
        );
        assert_eq!(fs::read_to_string(&journal)?, before);
        assert_eq!(store.list(&tenant).await?, updated);
    }
    fs::write(
        &env_cue,
        configuration.replace("name: \"outside\"", "naem: \"outside\""),
    )?;
    let output = lifecycle.execute(&["plan"], &[])?;
    let printed = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.status.code(), Some(3), "{printed}");
    assert!(
        printed.contains("CUE infrastructure configuration could not be evaluated"),
        "{printed}"
    );
    assert!(!printed.contains("naem"), "{printed}");
    assert_eq!(
        fs::read_to_string(&journal)?,
        before,
        "invalid generated configuration reached the provider"
    );
    fs::write(&env_cue, &configuration)?;

    // A read can return sensitive state together with diagnostics. The value
    // must be registered before the diagnostic reaches the JSON CLI envelope.
    fs::write(lifecycle.directory.path().join("lookup-error"), "")?;
    let output = lifecycle.execute(&["plan"], &[])?;
    assert!(!output.status.success());
    let printed = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(printed.contains("lookup failed"), "{printed}");
    assert!(!printed.contains(secret), "{printed}");
    assert!(!lifecycle.transcript()?.contains(secret));
    assert_eq!(store.list(&tenant).await?, updated);
    fs::remove_file(lifecycle.directory.path().join("lookup-error"))?;
    lifecycle.run(&["destroy", "--yes"])?;
    assert!(store.list(&tenant).await?.is_empty());
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the fake provider and Turso/sqld; see module documentation"]
async fn data_source_only_cli_invocations_read_without_managed_state() -> TestResult {
    let provider =
        PathBuf::from(std::env::var("CUENV_INFRASTRUCTURE_TEST_FAKE_PROVIDER")?).canonicalize()?;
    let database = Database::start()?;
    let backend = TursoConfiguration {
        url: database.url().to_owned(),
        authentication_token: std::env::var("TURSO_AUTH_TOKEN").ok(),
    };
    let directory = prepare_directory()?;
    let project = format!("data-only-{}", uuid::Uuid::new_v4());
    let tenant = TenantKey::try_new(MODULE, &project)?;
    let store = TursoStateStore::try_new(backend.clone())?;
    // This read-only/data-only path does not create the state schema. Set it
    // up explicitly instead of depending on another concurrent test to do so.
    store.migrate().await?;
    let configuration = format!(
        r#"package examples
import "github.com/cuenv/cuenv/schema"
schema.#Project
name: {project}
infrastructure: {{
 state: turso: {{url: {url}, token: {{cuenvPassthrough: true, name: "TURSO_AUTH_TOKEN"}}}}
 providers: fake: {{source: "example.com/test/fake", path: {provider}, configuration: {{directory: {directory}}}}}
 data: lookup: {{type: "fake_lookup", configuration: {{name: "outside"}}}}
}}
"#,
        project = serde_json::to_string(&project)?,
        url = serde_json::to_string(&backend.url)?,
        provider = serde_json::to_string(&provider)?,
        directory = serde_json::to_string(directory.path())?,
    );
    let lifecycle = Lifecycle::from_configuration(directory, configuration, &backend)?;
    let mut previous_reads = 0;
    for arguments in [&["plan"][..], &["plan"][..], &["apply", "--yes"][..]] {
        let result = lifecycle.run(arguments)?;
        assert_eq!(
            result["dataSources"],
            serde_json::json!([{
                "address": "data.fake_lookup.lookup", "readAt": "plan",
            }])
        );
        assert_eq!(result["changes"], serde_json::json!([]));
        let journal = fs::read_to_string(lifecycle.directory.path().join("journal.log"))?;
        let reads = journal.matches("lookup: Read name=outside").count();
        assert!(
            reads > previous_reads,
            "each invocation must read again: {journal}"
        );
        previous_reads = reads;
        assert!(store.list(&tenant).await?.is_empty());
        assert_eq!(
            lifecycle.run(&["state", "list"])?["resources"],
            serde_json::json!([])
        );
    }
    // A sensitive map's keys are values too. Echo one in a fresh-process
    // diagnostic; registering the unrelated secret scalar cannot hide it.
    let secret_key = format!(
        "lookup-map-key-{}",
        lifecycle
            .directory
            .path()
            .file_name()
            .ok_or("no directory name")?
            .to_string_lossy()
    );
    fs::write(lifecycle.directory.path().join("lookup-map-error"), "")?;
    let failed = lifecycle.execute(&["plan"], &[])?;
    let diagnostic = format!(
        "{}{}",
        String::from_utf8_lossy(&failed.stdout),
        String::from_utf8_lossy(&failed.stderr)
    );
    assert!(!failed.status.success(), "{diagnostic}");
    assert!(
        diagnostic.contains("sensitive map key rejected"),
        "{diagnostic}"
    );
    assert!(!diagnostic.contains(&secret_key), "{diagnostic}");
    assert!(!lifecycle.transcript()?.contains(&secret_key));
    assert!(store.list(&tenant).await?.is_empty());
    Ok(())
}

/// Each command is a fresh process with an empty redaction registry. Once the
/// declarations are removed, destroy must recover the lookup-derived sensitivity
/// from managed state rather than reading the lookup or rediscovering references.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the fake provider and Turso/sqld; see module documentation"]
async fn data_source_sensitive_output_stays_redacted_in_fresh_process_destroy() -> TestResult {
    sensitive_data_source_destroy(SensitiveSource::Create).await
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the fake provider and Turso/sqld; see module documentation"]
async fn malformed_update_retains_data_source_sensitivity_for_fresh_process_destroy() -> TestResult
{
    sensitive_data_source_destroy(SensitiveSource::MalformedUpdate).await
}

enum SensitiveSource {
    Create,
    MalformedUpdate,
}

async fn sensitive_data_source_destroy(source: SensitiveSource) -> TestResult {
    let provider =
        PathBuf::from(std::env::var("CUENV_INFRASTRUCTURE_TEST_FAKE_PROVIDER")?).canonicalize()?;
    let database = Database::start()?;
    let backend = TursoConfiguration {
        url: database.url().to_owned(),
        authentication_token: std::env::var("TURSO_AUTH_TOKEN").ok(),
    };
    let directory = prepare_directory()?;
    let secret = format!(
        "lookup-secret-{}",
        directory
            .path()
            .file_name()
            .ok_or("no directory name")?
            .to_string_lossy()
    );
    let project = format!("data-source-destroy-secret-{}", uuid::Uuid::new_v4());
    let tenant = TenantKey::try_new(MODULE, &project)?;
    let store = TursoStateStore::try_new(backend.clone())?;
    let declarations = r#"
 data: lookup: {type: "fake_lookup", configuration: {name: "private"}, outputs: secret_value: string}
 resources: consumer: {
  type: "fake_plain"
  configuration: name: infrastructure.data.lookup.outputs.secret_value
 }
"#;
    let configuration = format!(
        r#"package examples
import "github.com/cuenv/cuenv/schema"
schema.#Project
name: {project}
infrastructure: {{
 state: turso: {{url: {url}, token: {{cuenvPassthrough: true, name: "TURSO_AUTH_TOKEN"}}}}
 providers: fake: {{source: "example.com/test/fake", path: {provider}, configuration: {{directory: {directory}}}}}
 {declarations}
}}
"#,
        project = serde_json::to_string(&project)?,
        url = serde_json::to_string(&backend.url)?,
        provider = serde_json::to_string(&provider)?,
        directory = serde_json::to_string(directory.path())?,
    );
    let initial_configuration = match source {
        SensitiveSource::Create => configuration.clone(),
        SensitiveSource::MalformedUpdate => configuration.replace(
            "infrastructure.data.lookup.outputs.secret_value",
            r#""public-before-update""#,
        ),
    };
    let lifecycle = Lifecycle::from_configuration(directory, initial_configuration, &backend)?;
    assert_eq!(lifecycle.run(&["apply", "--yes"])?["applied"]["create"], 1);
    if matches!(source, SensitiveSource::MalformedUpdate) {
        assert!(
            store.list(&tenant).await?[0]
                .sensitive_attributes
                .is_empty()
        );
        fs::write(lifecycle.directory.path().join("env.cue"), &configuration)?;
        fs::write(
            lifecycle.directory.path().join("malformed-update-result"),
            "",
        )?;
        let update = lifecycle.execute(&["apply", "--yes"], &[])?;
        assert!(!update.status.success(), "malformed update must fail");
        assert!(
            fs::read_to_string(lifecycle.directory.path().join("journal.log"))?
                .contains("plain: Update committed remotely but returned malformed state")
        );
        fs::remove_file(lifecycle.directory.path().join("malformed-update-result"))?;
    }
    let rows = store.list(&tenant).await?;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].address.to_string(), "fake_plain.consumer");
    match source {
        SensitiveSource::Create => assert_eq!(rows[0].state["name"], secret),
        SensitiveSource::MalformedUpdate => {
            assert_eq!(rows[0].state["name"], "public-before-update");
        }
    }
    assert!(!lifecycle.transcript()?.contains(&secret));

    // fake_plain.name is not sensitive in the provider schema. With both the
    // lookup and consumer absent, only persisted sensitivity can mask this old
    // value in a removal plan or a destroy run in a different process.
    fs::write(
        lifecycle.directory.path().join("env.cue"),
        configuration.replace(declarations, "resources: {}"),
    )?;
    let journal = lifecycle.directory.path().join("journal.log");
    let previous_reads = fs::read_to_string(&journal)?
        .matches("lookup: Read")
        .count();
    assert!(previous_reads > 0);
    let deletion_plan = lifecycle.run(&["plan"])?;
    assert_eq!(deletion_plan["summary"]["delete"], 1);
    assert!(!deletion_plan.to_string().contains(&secret));
    assert!(
        !lifecycle.transcript()?.contains(&secret),
        "fresh-process removal plan leaked the lookup secret"
    );
    assert_eq!(store.list(&tenant).await?, rows);

    let removal = lifecycle.execute_text(&["plan"])?;
    let removal_text = format!(
        "{}{}",
        String::from_utf8_lossy(&removal.stdout),
        String::from_utf8_lossy(&removal.stderr)
    );
    assert!(removal.status.success(), "{removal_text}");
    assert!(removal_text.contains("(sensitive)"), "{removal_text}");
    assert!(!removal_text.contains(&secret), "{removal_text}");

    // Force a diagnostic containing the copied value. This new process must
    // register stored sensitivity before refreshing an undeclared resource.
    let diagnostic_flag = lifecycle.directory.path().join("echo-read-name");
    fs::write(&diagnostic_flag, "")?;
    let refused = lifecycle.execute_text(&["destroy", "--yes"])?;
    let diagnostic = format!(
        "{}{}",
        String::from_utf8_lossy(&refused.stdout),
        String::from_utf8_lossy(&refused.stderr)
    );
    assert!(!refused.status.success(), "{diagnostic}");
    assert!(diagnostic.contains("stored name rejected"), "{diagnostic}");
    assert!(!diagnostic.contains(&secret), "{diagnostic}");
    assert_eq!(store.list(&tenant).await?, rows);
    fs::remove_file(diagnostic_flag)?;

    let destroyed = lifecycle.execute_text(&["destroy", "--yes"])?;
    let destroy_text = format!(
        "{}{}",
        String::from_utf8_lossy(&destroyed.stdout),
        String::from_utf8_lossy(&destroyed.stderr)
    );
    assert!(destroyed.status.success(), "{destroy_text}");
    assert!(destroy_text.contains("(sensitive)"), "{destroy_text}");
    assert!(!destroy_text.contains(&secret), "{destroy_text}");
    assert!(
        !lifecycle.transcript()?.contains(&secret),
        "fresh-process destroy leaked the lookup secret"
    );
    assert_eq!(
        fs::read_to_string(&journal)?
            .matches("lookup: Read")
            .count(),
        previous_reads
    );
    assert!(store.list(&tenant).await?.is_empty());
    Ok(())
}

// ---------------------------------------------------------------------------
// Native output references through the real CLI, CUE bridge and fake provider.
// ---------------------------------------------------------------------------

const OUTPUT_STACK: &str = r#"package stack
import (
	"encoding/json"
	"strings"
)
Marker: 1
#Parts: {in: string, out: strings.Split(in, ",")}
#Decoded: {in: string, out: json.Unmarshal(in)}
#Good: {
	resources: {
		producer: {type: "fake_plain", outputs: id: string, configuration: name: "producer"}
		consumer: {type: "fake_plain", configuration: name: _i & =~"^."}
	}
	_i: resources.producer.outputs.id
	...
}
#Bad: {
	resources: {
		producer: {type: "fake_plain", outputs: id: string, configuration: name: "producer"}
		consumer: {type: "fake_plain", configuration: {if (_i & "x") == "x" {name: "consumer"}}}
	}
	_i: resources.producer.outputs.id
	...
}
#Conflict: {
	resources: {
		producer: {type: "fake_plain", outputs: id: string, configuration: name: "producer"}
		consumer: {type: "fake_plain", configuration: name: _i & "not-the-output"}
	}
	_i: resources.producer.outputs.id
	...
}
"#;
const OUTPUT_PRODUCER: &str =
    r#"producer: {type: "fake_plain", outputs: id: string, configuration: name: "producer"}"#;
const PRODUCER_ID: &str = "infrastructure.resources.producer.outputs.id";

struct OutputLifecycle {
    lifecycle: Lifecycle,
    store: TursoStateStore,
    tenant: TenantKey,
    _database: Database,
}

impl OutputLifecycle {
    /// A project whose body uses `@INFRASTRUCTURE@` for state and the fake provider.
    fn new(label: &str, body: &str) -> TestResult<Self> {
        let provider = PathBuf::from(std::env::var("CUENV_INFRASTRUCTURE_TEST_FAKE_PROVIDER")?)
            .canonicalize()?;
        let database = Database::start()?;
        let backend = TursoConfiguration {
            url: database.url().to_owned(),
            authentication_token: std::env::var("TURSO_AUTH_TOKEN").ok(),
        };
        let directory = prepare_directory()?;
        fs::create_dir_all(directory.path().join("stack"))?;
        fs::write(directory.path().join("stack/stack.cue"), OUTPUT_STACK)?;
        let project = format!("{label}-{}", uuid::Uuid::new_v4());
        let infrastructure = format!(
            "state: turso: {{url: {url}, token: {{cuenvPassthrough: true, name: \"TURSO_AUTH_TOKEN\"}}}}\n providers: fake: {{source: \"example.com/test/fake\", path: {provider}, configuration: directory: {directory}}}",
            url = serde_json::to_string(&backend.url)?,
            provider = serde_json::to_string(&provider)?,
            directory = serde_json::to_string(directory.path())?,
        );
        let configuration = format!(
            "package examples\nimport (\n \"github.com/cuenv/cuenv/schema\"\n \"github.com/cuenv/cuenv/stack\"\n)\nschema.#Project\nname: {project}\n_marker: stack.Marker\n{body}\n",
            project = serde_json::to_string(&project)?,
            body = body.replace("@INFRASTRUCTURE@", &infrastructure),
        );
        let tenant = TenantKey::try_new(MODULE, &project)?;
        let store = TursoStateStore::try_new(backend.clone())?;
        let lifecycle = Lifecycle::from_configuration(directory, configuration, &backend)?;
        Ok(Self {
            lifecycle,
            store,
            tenant,
            _database: database,
        })
    }

    fn journal(&self) -> String {
        fs::read_to_string(self.lifecycle.directory.path().join("journal.log")).unwrap_or_default()
    }

    /// Names passed to fake_plain creates, in the order the provider saw them.
    fn creates(&self) -> Vec<String> {
        self.journal()
            .lines()
            .filter_map(|line| {
                line.split_once("Create name=")
                    .map(|(_, name)| name.to_owned())
            })
            .collect()
    }

    /// Anything the provider did to an object, across every fake resource type.
    fn mutations(&self) -> usize {
        self.journal()
            .lines()
            .filter(|line| {
                line.contains(": Create") || line.contains(": Update") || line.contains(": Delete")
            })
            .count()
    }

    async fn record(&self, name: &str) -> TestResult<cuenv_infrastructure::ManagedResource> {
        Ok(self
            .store
            .list(&self.tenant)
            .await?
            .into_iter()
            .find(|row| row.address.name == name)
            .ok_or_else(|| format!("{name} is not recorded"))?)
    }

    /// Run a command that must fail, for the reason named by `expected`.
    fn fails(&self, arguments: &[&str], expected: &str) -> TestResult<String> {
        let output = self.lifecycle.execute(arguments, &[])?;
        let printed = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            !output.status.success(),
            "unexpectedly succeeded: {printed}"
        );
        let flattened = printed.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(flattened.contains(expected), "wanted {expected}: {printed}");
        Ok(printed)
    }
}

fn outputs_stack(resources: &str) -> String {
    format!("infrastructure: {{\n @INFRASTRUCTURE@\n resources: {{{resources}}}\n}}")
}

/// #566: comprehensions unrelated to any output keep working, and a reference next
/// to them is unknown at plan, ordered, filled with the real value and recorded.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the fake provider and Turso/sqld; see module documentation"]
async fn output_references_coexist_with_unrelated_comprehensions() -> TestResult {
    let unrelated = r#"
_b: "x"
_z: {if _b == "x" {v: 1}}
_o?: string
_y: {if _o == _|_ {v: 1}}
_xs: ["a"]
_w: {for x in _xs {(x): 1}}
config: ci: cuenv: {source: "release", version: [if _o != _|_ {_o}, "0.63.1"][0]}
"#;
    let stack = OutputLifecycle::new(
        "unrelated-comprehensions",
        &format!(
            "{unrelated}{}",
            outputs_stack(&format!(
                "{OUTPUT_PRODUCER}, consumer: {{type: \"fake_plain\", configuration: name: {PRODUCER_ID}}}"
            ))
        ),
    )?;
    let planned = stack.lifecycle.execute_text(&["plan"])?;
    assert!(planned.status.success(), "{planned:?}");
    assert!(String::from_utf8_lossy(&planned.stdout).contains("known after apply"));
    assert_eq!(stack.mutations(), 0, "plan must not change anything");

    stack.lifecycle.run(&["apply", "--yes"])?;
    let creates = stack.creates();
    assert_eq!(creates.len(), 2, "{creates:?}");
    assert_eq!(creates[0], "producer");
    let consumer = stack.record("consumer").await?;
    let producer = stack.record("producer").await?;
    assert_eq!(consumer.dependencies, ["fake_plain.producer"]);
    assert_eq!(consumer.state["name"], producer.state["id"]);
    assert!(producer.dependencies.is_empty());

    stack.lifecycle.run(&["apply", "--yes"])?;
    assert_eq!(stack.mutations(), 2, "a converged stack must be a no-op");
    Ok(())
}

/// A hidden alias inside an imported definition keeps its edge and its constraint.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the fake provider and Turso/sqld; see module documentation"]
async fn imported_definitions_order_and_record_output_edges() -> TestResult {
    let stack = OutputLifecycle::new(
        "imported-alias",
        "infrastructure: stack.#Good & {\n @INFRASTRUCTURE@\n}",
    )?;
    stack.lifecycle.run(&["apply", "--yes"])?;
    assert_eq!(stack.creates()[0], "producer");
    let consumer = stack.record("consumer").await?;
    assert_eq!(consumer.dependencies, ["fake_plain.producer"]);
    assert_eq!(
        consumer.state["name"],
        stack.record("producer").await?.state["id"]
    );
    Ok(())
}

/// Preserved behaviour: a real output violating a consumer's constraint stops the
/// apply after the producer, and the consumer is never created.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the fake provider and Turso/sqld; see module documentation"]
async fn a_real_output_violating_a_constraint_stops_before_the_consumer() -> TestResult {
    let stack = OutputLifecycle::new(
        "constraint-violation",
        "infrastructure: stack.#Conflict & {\n @INFRASTRUCTURE@\n}",
    )?;
    stack.lifecycle.run(&["plan"])?;
    // The real value is checked as soon as it exists, before the consumer's turn.
    stack.fails(
        &["apply", "--yes"],
        "the real value of output fake_plain.producer.id",
    )?;
    assert_eq!(stack.creates(), ["producer"]);
    assert!(stack.record("consumer").await.is_err());
    Ok(())
}

/// Output-dependent structure is refused before the provider changes anything.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the fake provider and Turso/sqld; see module documentation"]
async fn output_dependent_structure_is_refused_before_any_provider_change() -> TestResult {
    let consumer = |configuration: &str| {
        outputs_stack(&format!(
            "{OUTPUT_PRODUCER}, consumer: {{type: \"fake_plain\", configuration: {configuration}}}"
        ))
    };
    let fake_object = outputs_stack(&format!(
        "{OUTPUT_PRODUCER}, consumer: {{type: \"fake_obj\", configuration: close({{key: \"consumer\", (*({PRODUCER_ID} & \"version\") | \"mode\"): \"value\"}})}}"
    ));
    let cases = [
        (
            "imported-conditional",
            "infrastructure: stack.#Bad & {\n @INFRASTRUCTURE@\n}".to_string(),
        ),
        ("close-dynamic-label", fake_object),
        (
            "default-marker",
            consumer(&format!(
                "*{{name: \"base\", if ({PRODUCER_ID} & =~\"^bad$\") != _|_ {{extra: \"e\"}}}} | {{name: \"z\"}}"
            )),
        ),
        (
            "plain-condition",
            consumer(&format!(
                "{{name: \"n\", if {PRODUCER_ID} == \"x\" {{extra: \"e\"}}}}"
            )),
        ),
        (
            "hidden-control-read-by-the-configuration",
            format!(
                "_o: {PRODUCER_ID}\n_h: {{if (_o & =~\"^x\") != _|_ {{v: 1}}}}\n{}",
                consumer("{name: \"n\", if _h.v != _|_ {extra: \"e\"}}")
            ),
        ),
        (
            "definition-as-function",
            format!(
                "#F: {{in: string, out: {{if (in & =~\"^x\") != _|_ {{extra: \"e\"}}}}}}\n{}",
                consumer(&format!(
                    "{{name: \"n\"}} & (#F & {{in: {PRODUCER_ID}}}).out"
                ))
            ),
        ),
    ];
    for (label, body) in &cases {
        let stack = OutputLifecycle::new(label, body)?;
        stack.fails(&["plan"], "an output cannot decide the structure")?;
        stack.fails(&["apply", "--yes"], "an output cannot decide the structure")?;
        assert_eq!(stack.mutations(), 0, "{label} changed: {}", stack.journal());
        assert!(stack.store.list(&stack.tenant).await?.is_empty(), "{label}");
        // The same syntax on an unrelated value must plan: the refusal above is about
        // the output, not a mistake in this fixture.
        if body.contains(PRODUCER_ID) {
            let unrelated = format!(
                "_unrelated: \"unmatched\"\n{}",
                body.replace(PRODUCER_ID, "_unrelated")
            );
            OutputLifecycle::new(label, &unrelated)?
                .lifecycle
                .run(&["plan"])?;
        }
    }
    Ok(())
}

/// Every way of writing a reference that CUE evaluates to the output itself carries it,
/// wherever it sits and however it got there. Each is applied through a real provider.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the fake provider and Turso/sqld; see module documentation"]
async fn every_way_of_reaching_an_output_is_ordered_and_filled() -> TestResult {
    let cases = [
        ("direct", String::new(), PRODUCER_ID.to_string()),
        (
            "constrained",
            String::new(),
            format!("{PRODUCER_ID} & =~\"^.\""),
        ),
        (
            "field-alias",
            format!("_alias: {PRODUCER_ID}\n"),
            "_alias".to_string(),
        ),
        ("let", format!("let L = {PRODUCER_ID}\n"), "L".to_string()),
        (
            "let-of-a-struct",
            "let P = infrastructure.resources.producer\n".to_string(),
            "P.outputs.id".to_string(),
        ),
        (
            "function",
            "#F: {in: string, v: in}\n".to_string(),
            format!("(#F & {{in: {PRODUCER_ID}}}).v"),
        ),
        (
            "inline-struct",
            String::new(),
            format!("({{in: {PRODUCER_ID}, v: in}}).v"),
        ),
        (
            "value-alias",
            format!("_s: W={{z: {PRODUCER_ID}, y: W.z}}\n"),
            "_s.y".to_string(),
        ),
        (
            "selected-from-a-list",
            String::new(),
            format!("[{PRODUCER_ID}][0]"),
        ),
    ];
    for (label, prefix, name) in &cases {
        let stack = OutputLifecycle::new(
            label,
            &format!(
                "{prefix}{}",
                outputs_stack(&format!(
                    "{OUTPUT_PRODUCER}, consumer: {{type: \"fake_plain\", configuration: name: {name}}}"
                ))
            ),
        )?;
        let planned = stack.lifecycle.execute_text(&["plan"])?;
        assert!(planned.status.success(), "{label}: {planned:?}");
        assert!(
            String::from_utf8_lossy(&planned.stdout).contains("known after apply"),
            "{label}"
        );
        stack.lifecycle.run(&["apply", "--yes"])?;
        assert_eq!(stack.creates()[0], "producer", "{label}");
        let consumer = stack.record("consumer").await?;
        assert_eq!(consumer.dependencies, ["fake_plain.producer"], "{label}");
        assert_eq!(
            consumer.state["name"],
            stack.record("producer").await?.state["id"],
            "{label}"
        );
    }
    Ok(())
}

/// Conditions and loops over what is known work around references: only what decides
/// the structure has to be known at plan.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the fake provider and Turso/sqld; see module documentation"]
async fn references_inside_known_conditions_and_loops_are_filled() -> TestResult {
    let conditional = OutputLifecycle::new(
        "conditional-body",
        &format!(
            "_enabled: true\n{}",
            outputs_stack(&format!(
                "{OUTPUT_PRODUCER}, consumer: {{type: \"fake_plain\", configuration: {{if _enabled {{name: {PRODUCER_ID}}}}}}}"
            ))
        ),
    )?;
    conditional.lifecycle.run(&["apply", "--yes"])?;
    assert_eq!(
        conditional.record("consumer").await?.state["name"],
        conditional.record("producer").await?.state["id"]
    );

    // One consumer per producer, named from a key and filled from the loop variable.
    let looped = OutputLifecycle::new(
        "loop-over-producers",
        &format!(
            "_producers: {{a: \"first\", b: \"second\"}}\n{}\ninfrastructure: resources: {{for key, _ in _producers {{\"consumer_\\(key)\": {{type: \"fake_plain\", configuration: name: infrastructure.resources[\"producer_\\(key)\"].outputs.id}}}}}}",
            outputs_stack(
                "for key, name in _producers {\"producer_\\(key)\": {type: \"fake_plain\", outputs: id: string, configuration: \"name\": name}}"
            )
        ),
    )?;
    looped.lifecycle.run(&["apply", "--yes"])?;
    for key in ["a", "b"] {
        let consumer = looped.record(&format!("consumer_{key}")).await?;
        assert_eq!(
            consumer.dependencies,
            [format!("fake_plain.producer_{key}")]
        );
        assert_eq!(
            consumer.state["name"],
            looped.record(&format!("producer_{key}")).await?.state["id"]
        );
    }
    Ok(())
}

/// A value computed from outputs is unknown until all of them are, is recorded as
/// depending on each, and converges.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the fake provider and Turso/sqld; see module documentation"]
async fn computed_values_wait_for_every_output_they_read() -> TestResult {
    let stack = OutputLifecycle::new(
        "computed",
        &format!(
            "_other: infrastructure.resources.other.outputs.id\n{}",
            outputs_stack(&format!(
                "{OUTPUT_PRODUCER}, other: {{type: \"fake_plain\", outputs: id: string, configuration: name: \"other\"}}, interpolated: {{type: \"fake_plain\", configuration: name: \"app-\\({PRODUCER_ID})-\\(_other)\"}}, concatenated: {{type: \"fake_plain\", configuration: name: {PRODUCER_ID} + \"-suffix\"}}"
            ))
        ),
    )?;
    let planned = stack.lifecycle.execute_text(&["plan"])?;
    assert!(planned.status.success(), "{planned:?}");
    assert!(String::from_utf8_lossy(&planned.stdout).contains("known after apply"));
    assert_eq!(stack.mutations(), 0, "plan must not change anything");

    stack.lifecycle.run(&["apply", "--yes"])?;
    let producer = stack.record("producer").await?;
    let other = stack.record("other").await?;
    let producer_id = producer.state["id"].as_str().ok_or("no id")?;
    let other_id = other.state["id"].as_str().ok_or("no id")?;
    let interpolated = stack.record("interpolated").await?;
    assert_eq!(
        interpolated.state["name"],
        format!("app-{producer_id}-{other_id}")
    );
    assert_eq!(
        interpolated.dependencies,
        ["fake_plain.other", "fake_plain.producer"]
    );
    let concatenated = stack.record("concatenated").await?;
    assert_eq!(concatenated.state["name"], format!("{producer_id}-suffix"));
    assert_eq!(concatenated.dependencies, ["fake_plain.producer"]);

    let mutations = stack.mutations();
    stack.lifecycle.run(&["apply", "--yes"])?;
    assert_eq!(
        stack.mutations(),
        mutations,
        "a converged stack must be a no-op"
    );
    Ok(())
}

/// CUE decides the shape of a computed value. A list cannot reach a destination that
/// holds a list, as references need a scalar, and at a scalar one the type stops the run.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the fake provider and Turso/sqld; see module documentation"]
async fn a_computed_value_that_is_not_a_scalar_is_refused() -> TestResult {
    let parts = format!("(stack.#Parts & {{in: {PRODUCER_ID}}}).out");
    let into_a_set = OutputLifecycle::new(
        "computed-list-destination",
        &outputs_stack(&format!(
            "{OUTPUT_PRODUCER}, consumer: {{type: \"fake_tags\", configuration: tags: {parts}}}"
        )),
    )?;
    into_a_set.fails(&["plan"], "references require a scalar destination")?;
    assert_eq!(into_a_set.mutations(), 0);

    let whole_configuration = OutputLifecycle::new(
        "computed-whole-configuration",
        &outputs_stack(&format!(
            "{OUTPUT_PRODUCER}, consumer: {{type: \"fake_plain\", configuration: (stack.#Decoded & {{in: {PRODUCER_ID}}}).out}}"
        )),
    )?;
    whole_configuration.fails(
        &["plan"],
        "output selectors require a resource or data source configuration argument",
    )?;
    assert_eq!(whole_configuration.mutations(), 0);

    let into_a_string = OutputLifecycle::new(
        "computed-list-scalar",
        &outputs_stack(&format!(
            "{OUTPUT_PRODUCER}, consumer: {{type: \"fake_plain\", configuration: name: {parts}}}"
        )),
    )?;
    into_a_string.lifecycle.run(&["plan"])?;
    into_a_string.fails(&["apply", "--yes"], "wrong type for its destination")?;
    assert_eq!(into_a_string.creates(), ["producer"]);
    assert!(into_a_string.record("consumer").await.is_err());
    Ok(())
}

/// A declared output is checked against the provider's schema while planning, whether or
/// not anything reads it: otherwise its value could only be refused once it exists.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the fake provider and Turso/sqld; see module documentation"]
async fn an_output_declared_with_the_wrong_type_is_refused_while_planning() -> TestResult {
    let stack = OutputLifecycle::new(
        "unread-output-wrong-type",
        &outputs_stack(
            r#"producer: {type: "fake_plain", outputs: id: number, configuration: name: "producer"}"#,
        ),
    )?;
    stack.fails(
        &["plan"],
        "native CUE output declaration type does not match the provider schema",
    )?;
    stack.fails(
        &["apply", "--yes"],
        "native CUE output declaration type does not match the provider schema",
    )?;
    assert_eq!(stack.mutations(), 0);
    Ok(())
}

/// A definition with a conditional field reading something outside of it used to
/// crash the evaluator as soon as it was instantiated.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the fake provider and Turso/sqld; see module documentation"]
async fn a_conditional_field_in_a_definition_reads_what_is_outside_it() -> TestResult {
    let stack = OutputLifecycle::new(
        "conditional-definition",
        &format!(
            "_name: \"from-outside\"\n#Settings: {{if true {{name: _name}}}}\n{}",
            outputs_stack("consumer: {type: \"fake_plain\", configuration: #Settings}")
        ),
    )?;
    stack.lifecycle.run(&["apply", "--yes"])?;
    assert_eq!(
        stack.record("consumer").await?.state["name"],
        "from-outside"
    );
    Ok(())
}

/// A check nothing in the configuration reads does not stop a plan, and a real value
/// violating it stops the apply before anything else changes.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the fake provider and Turso/sqld; see module documentation"]
async fn a_check_outside_the_configuration_is_verified_against_real_values() -> TestResult {
    let stack = OutputLifecycle::new(
        "unread-check",
        &format!(
            "_check: {PRODUCER_ID} & \"not-the-output\"\n{}",
            outputs_stack(&format!(
                "{OUTPUT_PRODUCER}, consumer: {{type: \"fake_plain\", configuration: name: {PRODUCER_ID}}}"
            ))
        ),
    )?;
    stack.lifecycle.run(&["plan"])?;
    stack.fails(
        &["apply", "--yes"],
        "the real value of output fake_plain.producer.id",
    )?;
    assert_eq!(stack.creates(), ["producer"]);
    assert!(stack.record("consumer").await.is_err());
    Ok(())
}

/// A sensitive output keeps a value computed from it out of plans and logs.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the fake provider and Turso/sqld; see module documentation"]
async fn a_value_computed_from_a_sensitive_output_stays_masked() -> TestResult {
    let stack = OutputLifecycle::new(
        "sensitive-computed",
        &token_stack(
            "\"bearer \\(infrastructure.resources.producer.outputs.token)\"",
            "source",
        ),
    )?;
    let planned = stack.lifecycle.execute_text(&["plan"])?;
    assert!(planned.status.success(), "{planned:?}");
    let applied = stack.lifecycle.execute_text(&["apply", "--yes"])?;
    assert!(applied.status.success(), "{applied:?}");
    let producer = stack.record("producer").await?;
    let consumer = stack.record("consumer").await?;
    let token = producer.state["token"].as_str().ok_or("no token")?;
    assert_eq!(consumer.state["config"]["token"], format!("bearer {token}"));
    assert_eq!(consumer.sensitive_attributes, ["config"]);
    stack.lifecycle.execute_text(&["plan"])?;
    let transcript = stack.lifecycle.transcript()?;
    assert!(!transcript.contains(token));
    assert!(!transcript.contains(&format!("bearer {token}")));
    Ok(())
}

/// An output declared as a scalar whose real value is a list stops the run by name.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the fake provider and Turso/sqld; see module documentation"]
async fn a_declared_scalar_output_with_a_list_value_stops_the_run() -> TestResult {
    let stack = OutputLifecycle::new(
        "non-scalar-output",
        &outputs_stack(
            "producer: {type: \"fake_dyn\", outputs: data: string, configuration: name: \"producer\"}, later: {type: \"fake_plain\", dependsOn: [\"producer\"], configuration: name: \"later\"}",
        ),
    )?;
    stack.fails(
        &["apply", "--yes"],
        "fake_dyn.producer.data is declared as a scalar",
    )?;
    assert!(!stack.creates().contains(&"later".to_string()));
    assert!(stack.record("later").await.is_err());
    Ok(())
}

/// A default over an absent optional field reads nothing, and stays ordinary CUE.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the fake provider and Turso/sqld; see module documentation"]
async fn a_default_over_an_absent_optional_field_is_ordinary_cue() -> TestResult {
    let stack = OutputLifecycle::new(
        "optional-fallback",
        &format!(
            "_x: {{a?: string}}\n{}",
            outputs_stack(
                "consumer: {type: \"fake_plain\", configuration: name: *_x.a | \"fallback\"}"
            )
        ),
    )?;
    stack.lifecycle.run(&["apply", "--yes"])?;
    assert_eq!(stack.record("consumer").await?.state["name"], "fallback");
    Ok(())
}

fn token_stack(token: &str, source: &str) -> String {
    outputs_stack(&format!(
        "producer: {{type: \"fake_token_source\", outputs: token: string, configuration: name: \"{source}\"}}, consumer: {{type: \"fake_token_consumer\", configuration: config: {{token: {token}, label: \"ordinary-sibling\"}}}}"
    ))
}

/// A sensitive output reaches a nested destination, is marked sensitive, recorded
/// as a dependency, and never printed.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the fake provider and Turso/sqld; see module documentation"]
async fn sensitive_nested_destinations_stay_masked_and_recorded() -> TestResult {
    let stack = OutputLifecycle::new(
        "sensitive-nested",
        &token_stack("infrastructure.resources.producer.outputs.token", "source"),
    )?;
    let planned = stack.lifecycle.execute_text(&["plan"])?;
    assert!(planned.status.success(), "{planned:?}");
    let applied = stack.lifecycle.execute_text(&["apply", "--yes"])?;
    assert!(applied.status.success(), "{applied:?}");
    let producer = stack.record("producer").await?;
    let consumer = stack.record("consumer").await?;
    let token = producer.state["token"].as_str().ok_or("no token")?;
    assert!(!token.is_empty());
    assert_eq!(consumer.state["config"]["token"], token);
    assert_eq!(consumer.dependencies, ["fake_token_source.producer"]);
    assert_eq!(consumer.sensitive_attributes, ["config"]);
    stack.lifecycle.execute_text(&["plan"])?;
    assert!(!stack.lifecycle.transcript()?.contains(token));
    Ok(())
}

/// A replacement first revealed by the final plan stops before the consumer changes.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the fake provider and Turso/sqld; see module documentation"]
async fn final_plan_replacement_leaves_the_consumer_unchanged() -> TestResult {
    let stack = OutputLifecycle::new(
        "late-replacement",
        &token_stack("\"ordinary-existing-token\"", "first"),
    )?;
    stack.lifecycle.run(&["apply", "--yes"])?;
    let before = stack.record("consumer").await?;
    fs::write(
        stack
            .lifecycle
            .directory
            .path()
            .join("token-consumer-late-replace"),
        b"",
    )?;
    let edited = stack
        .lifecycle
        .configuration
        .replace(
            "\"ordinary-existing-token\"",
            "infrastructure.resources.producer.outputs.token",
        )
        .replace("name: \"first\"", "name: \"second\"");
    fs::write(stack.lifecycle.directory.path().join("env.cue"), edited)?;
    stack.fails(&["apply", "--yes"], "not in the confirmed plan")?;
    let after = stack.record("consumer").await?;
    assert_eq!(before.state, after.state);
    assert_eq!(before.generation, after.generation);
    Ok(())
}

/// Each step of a chain specializes from the same snapshot with exact edges.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the fake provider and Turso/sqld; see module documentation"]
async fn output_chains_specialize_in_order_with_exact_edges() -> TestResult {
    let stack = OutputLifecycle::new(
        "output-chain",
        &outputs_stack(&format!(
            "{OUTPUT_PRODUCER}, middle: {{type: \"fake_plain\", outputs: id: string, configuration: name: {PRODUCER_ID}}}, last: {{type: \"fake_plain\", configuration: name: infrastructure.resources.middle.outputs.id}}"
        )),
    )?;
    stack.lifecycle.run(&["apply", "--yes"])?;
    assert_eq!(stack.creates()[0], "producer");
    let producer = stack.record("producer").await?;
    let middle = stack.record("middle").await?;
    let last = stack.record("last").await?;
    assert_eq!(middle.dependencies, ["fake_plain.producer"]);
    assert_eq!(last.dependencies, ["fake_plain.middle"]);
    assert_eq!(middle.state["name"], producer.state["id"]);
    assert_eq!(last.state["name"], middle.state["id"]);
    Ok(())
}

/// A separate infrastructure package follows the same contract as an inline one.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires the fake provider and Turso/sqld; see module documentation"]
async fn separate_infrastructure_packages_share_the_output_contract() -> TestResult {
    let stack = OutputLifecycle::new(
        "separate-package",
        &outputs_stack(&format!(
            "{OUTPUT_PRODUCER}, consumer: {{type: \"fake_plain\", configuration: name: {PRODUCER_ID}}}"
        )),
    )?;
    let directory = stack.lifecycle.directory.path();
    let (project, infrastructure) = stack
        .lifecycle
        .configuration
        .split_once("infrastructure: {")
        .ok_or("missing infrastructure declaration")?;
    fs::write(
        directory.join("env.cue"),
        project
            .replace(" \"github.com/cuenv/cuenv/stack\"\n", "")
            .replace("_marker: stack.Marker\n", ""),
    )?;
    fs::create_dir(directory.join("infra"))?;
    fs::write(
        directory.join("infra/stack.cue"),
        format!(
            "package infrastructure\n_b: \"x\"\n_z: {{if _b == \"x\" {{v: 1}}}}\ninfrastructure: {{{infrastructure}"
        ),
    )?;
    fs::write(
        directory.join(".infrastructure.cue"),
        "package examples\ndirectory: \"infra\"\n\"package\": \"infrastructure\"\nprojectPackage: \"examples\"\n",
    )?;
    stack.lifecycle.run(&["apply", "--yes"])?;
    assert_eq!(stack.creates()[0], "producer");
    assert_eq!(
        stack.record("consumer").await?.dependencies,
        ["fake_plain.producer"]
    );
    fs::write(
        directory.join("infra/control.cue"),
        format!(
            "package infrastructure\n_o: {PRODUCER_ID}\ninfrastructure: resources: consumer: configuration: {{if (_o & \"x\") == \"x\" {{extra: \"e\"}}}}\n"
        ),
    )?;
    let before = stack.mutations();
    stack.fails(&["plan"], "an output cannot decide the structure")?;
    assert_eq!(stack.mutations(), before);
    Ok(())
}
