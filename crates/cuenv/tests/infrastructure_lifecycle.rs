//! Random provider lifecycle through the CLI, checked-out CUE example and Turso.
//!
//! Two tests run the CLI against a real provider and state server: the legacy
//! no-flag stack from the checked-out example, and a named `--env dev` stack
//! whose provider receives an `#ExecSecret` through `allowInfrastructure`.
//!
//! These tests are ignored because they need a real provider binary and state server:
//! ```text
//! CUENV_INFRASTRUCTURE_TEST_RANDOM_PROVIDER=/path/terraform-provider-random \
//! CUENV_INFRASTRUCTURE_TEST_TURSO_URL=http://127.0.0.1:8080 \
//! cuenv exec -- cargo test -p cuenv --test infrastructure_lifecycle -- --ignored
//! ```
//! Start a local server with `sqld --http-listen-addr 127.0.0.1:8080`.
//! `TURSO_AUTH_TOKEN` is optional for an authenticated test database. Each run
//! uses a unique project name and temporary configuration and user directories.

use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Mutex;

use cuenv_infrastructure::{StateStore, TenantKey, TursoConfiguration, TursoStateStore};
use serde_json::Value;

type TestResult<Success = ()> = Result<Success, Box<dyn Error>>;

const MODULE: &str = "github.com/cuenv/cuenv";
const PET_LENGTH: &str = "length:    2";
const SECRET_VARIABLE: &str = "DEPLOY_TOKEN";
const SECRET_VALUE: &str = "lifecycle-secret-4f9c2d71-never-printed";
const RECORDED_SECRET: &str = "recorded-secret";

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
                "\"http://127.0.0.1:8080\"",
                &serde_json::to_string(&backend.url)?,
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
	state: turso: url: {url}
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

    fn recorded_secret(&self) -> TestResult<String> {
        Ok(fs::read_to_string(
            self.directory.path().join(RECORDED_SECRET),
        )?)
    }

    /// Run the CLI and record everything it printed.
    fn execute(&self, arguments: &[&str]) -> TestResult<Output> {
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
            .current_dir(self.directory.path())
            .args(["--json", "infrastructure"])
            .args(arguments)
            .args(["--package", "examples"]);
        if let Some(token) = &self.authentication_token {
            command.env("TURSO_AUTH_TOKEN", token);
        }
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
        let output = self.execute(arguments)?;
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
        let output = self.execute(arguments)?;
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
    let backend = TursoConfiguration {
        url: std::env::var("CUENV_INFRASTRUCTURE_TEST_TURSO_URL")?,
        authentication_token: std::env::var("TURSO_AUTH_TOKEN").ok(),
    };
    let store = TursoStateStore::new(backend.clone())?;
    let project = format!("random-lifecycle-{}", uuid::Uuid::new_v4());
    let tenant = TenantKey::new(MODULE, &project)?;
    let lifecycle = Lifecycle::new(&provider, &backend, &project)?;

    let plan = lifecycle.run(&["plan"])?;
    assert_eq!(plan["summary"]["create"], 3);
    assert!(
        store.list(&tenant).await?.is_empty(),
        "plan wrote resources"
    );
    assert!(
        store.owner(&tenant).await?.is_none(),
        "plan claimed ownership"
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
    let backend = TursoConfiguration {
        url: std::env::var("CUENV_INFRASTRUCTURE_TEST_TURSO_URL")?,
        authentication_token: std::env::var("TURSO_AUTH_TOKEN").ok(),
    };
    let store = TursoStateStore::new(backend.clone())?;
    let project = format!("random-secret-lifecycle-{}", uuid::Uuid::new_v4());
    let tenant = TenantKey::with_environment(MODULE, &project, "dev")?;
    // A fresh project name means the no-flag namespace holds no rows to migrate.
    let legacy = TenantKey::new(MODULE, &project)?;
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

/// One project at the two stages of moving a resource from an environment to
/// the top level: `dev` records `random_pet.pet`, then the top level declares
/// the same address while `dev` declares another resource.
fn identity_conflict_configuration(
    provider: &Path,
    backend: &TursoConfiguration,
    project: &str,
    top_level_pet: bool,
) -> TestResult<String> {
    let provider_block = format!(
        "providers: random: {{source: \"hashicorp/random\", path: {}}}",
        serde_json::to_string(provider)?
    );
    let pet = "pet: {type: \"random_pet\", configuration: {length: 2, separator: \"-\"}}";
    let (top_level, environment_resource) = if top_level_pet {
        (
            format!("{provider_block}\n\tresources: {pet}"),
            "identifier: {type: \"random_id\", configuration: byte_length: 4}",
        )
    } else {
        (String::new(), pet)
    };
    Ok(format!(
        r#"package examples

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: {project}

infrastructure: {{
	state: turso: url: {url}
	{top_level}
	environments: dev: {{
		{provider_block}
		resources: {environment_resource}
	}}
}}
"#,
        project = serde_json::to_string(project)?,
        url = serde_json::to_string(&backend.url)?,
    ))
}

/// A run without `--env` must not create an address a declared environment
/// already records: both identities would manage one real object, and a later
/// `destroy --env dev` would delete what the top-level configuration manages.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires a real Random provider and Turso/sqld; see module documentation"]
async fn a_run_without_env_cannot_claim_what_an_environment_records() -> TestResult {
    let provider = PathBuf::from(std::env::var("CUENV_INFRASTRUCTURE_TEST_RANDOM_PROVIDER")?)
        .canonicalize()?;
    let backend = TursoConfiguration {
        url: std::env::var("CUENV_INFRASTRUCTURE_TEST_TURSO_URL")?,
        authentication_token: std::env::var("TURSO_AUTH_TOKEN").ok(),
    };
    let store = TursoStateStore::new(backend.clone())?;
    let project = format!("random-identity-{}", uuid::Uuid::new_v4());
    let unselected = TenantKey::new(MODULE, &project)?;
    let dev = TenantKey::with_environment(MODULE, &project, "dev")?;
    let lifecycle = Lifecycle::from_configuration(
        prepare_directory()?,
        identity_conflict_configuration(&provider, &backend, &project, false)?,
        &backend,
    )?;
    let created = lifecycle.run(&["apply", "--env", "dev", "--yes"])?;
    assert_eq!(created["applied"]["create"], 1);

    // The pet moves to the top level; `dev` keeps declaring something else.
    fs::write(
        lifecycle.directory.path().join("env.cue"),
        identity_conflict_configuration(&provider, &backend, &project, true)?,
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
