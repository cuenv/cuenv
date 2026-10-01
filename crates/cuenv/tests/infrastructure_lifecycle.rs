//! Random provider lifecycle through the CLI, checked-out CUE example and Turso.
//!
//! This test is ignored because it needs a real provider binary and state server:
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
use std::process::Command;

use cuenv_infrastructure::{StateStore, TenantKey, TursoConfiguration, TursoStateStore};
use serde_json::Value;

type TestResult<Success = ()> = Result<Success, Box<dyn Error>>;

const MODULE: &str = "github.com/cuenv/cuenv";
const PET_LENGTH: &str = "length:    2";

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
}

impl Lifecycle {
    fn new(provider: &Path, backend: &TursoConfiguration, project: &str) -> TestResult<Self> {
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
        fs::write(directory.path().join("env.cue"), &configuration)?;
        Ok(Self {
            directory,
            configuration,
            authentication_token: backend.authentication_token.clone(),
        })
    }

    fn run(&self, arguments: &[&str]) -> TestResult<Value> {
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
        let output = command.output()?;
        assert!(
            output.status.success(),
            "cuenv infrastructure {} exited {}: {}",
            arguments.join(" "),
            output.status,
            String::from_utf8_lossy(&output.stderr),
        );
        // Parsing the complete stdout also rejects extra JSON documents or logs.
        let envelope: Value = serde_json::from_slice(&output.stdout)?;
        assert_eq!(envelope["status"], "ok");
        Ok(envelope["data"].clone())
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
