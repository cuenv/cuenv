//! Tests that evaluate this repository's own CUE files through the bridge.
//!
//! The repository is the largest real CUE module the bridge sees in tests:
//! every example and schema file must keep evaluating, and the
//! infrastructure schema must keep rejecting mistakes (misspelled fields,
//! undeclared references, an invalid state URL) when a project embeds
//! `schema.#Project` at file level (the common way to write `env.cue`).
//! The semantic checks use `error()`, which needs CUE language v0.14 in the
//! module that holds the schema; the copy of `cue.mod` made here has it.
//!
//! Each test copies the CUE files it needs into a temporary module instead
//! of evaluating the checkout in place. The checkout's `target/` directory
//! holds build output and fixtures that other test binaries write while
//! this one runs, which a recursive load would otherwise pick up.

use cuengine::{InstanceFailures, ModuleEvalOptions, ModuleResult, PackageScope, evaluate_module};
use cuenv_core::environment::EnvValue;
use cuenv_core::manifest::{
    Infrastructure, InfrastructurePolicyAction, Project, ProviderEnvironment,
};
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

fn repository_root() -> TestResult<PathBuf> {
    Ok(Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?)
}

fn temporary_module(prefix: &str) -> TestResult<TempDir> {
    Ok(tempfile::Builder::new().prefix(prefix).tempdir()?)
}

/// Directories never copied: build output, dependencies, and trees the CUE
/// loader does not walk anyway (names starting with `.` or `_`).
fn is_copied_directory(name: &str) -> bool {
    !(name == "target" || name == "node_modules" || name.starts_with('.') || name.starts_with('_'))
}

/// Copy every `.cue` file below `source` (and all of `cue.mod`) to
/// `destination`, keeping relative paths.
fn copy_cue_files(source: &Path, destination: &Path) -> TestResult {
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            if name == "cue.mod" || is_copied_directory(&name) {
                copy_cue_files(&path, &destination.join(name.as_ref()))?;
            }
        } else if file_type.is_file()
            && (path.extension().is_some_and(|extension| extension == "cue")
                || source.ends_with("cue.mod"))
        {
            fs::create_dir_all(destination)?;
            fs::copy(&path, destination.join(name.as_ref()))?;
        }
    }
    Ok(())
}

#[test]
fn every_instance_of_the_repository_evaluates() -> TestResult {
    // The strictest module-wide evaluation, as `cuenv infrastructure plan`
    // runs it: every package of every directory, and any failure fails the
    // call. A broken example (for example a task reference that no longer
    // resolves) makes this test fail instead of landing silently.
    let repository = repository_root()?;
    let module = temporary_module("cuengine-repository-")?;
    copy_cue_files(&repository, module.path())?;

    let options = ModuleEvalOptions {
        recursive: true,
        package_scope: PackageScope::All,
        instance_failures: InstanceFailures::Fail,
        ..Default::default()
    };
    let result = evaluate_module(module.path(), "", Some(&options))?;
    assert!(
        result.instances.contains_key(".:cuenv"),
        "the repository's own env.cue is missing: {:?}",
        result.instances.keys().collect::<Vec<_>>()
    );
    assert!(
        result
            .instances
            .keys()
            .any(|key| key.starts_with("examples/")),
        "no example was evaluated"
    );
    Ok(())
}

/// A module holding a copy of the repository schema and one project that
/// embeds `schema.#Project` at file level.
struct SchemaFixture {
    module: TempDir,
}

impl SchemaFixture {
    fn new(body: &str) -> TestResult<Self> {
        let repository = repository_root()?;
        let module = temporary_module("cuengine-infrastructure-schema-")?;
        copy_cue_files(&repository.join("cue.mod"), &module.path().join("cue.mod"))?;
        copy_cue_files(&repository.join("schema"), &module.path().join("schema"))?;
        let project = module.path().join("app");
        fs::create_dir_all(&project)?;
        fs::write(
            project.join("env.cue"),
            format!(
                "package app\n\nimport \"github.com/cuenv/cuenv/schema\"\n\nschema.#Project\n\nname: \"schema-fixture\"\n\n{body}\n"
            ),
        )?;
        Ok(Self { module })
    }

    /// Evaluate the project as `cuenv infrastructure` evaluates its target.
    fn evaluate(&self) -> cuengine::Result<ModuleResult> {
        let options = ModuleEvalOptions {
            package_name: Some("app".to_string()),
            target_dir: Some(self.module.path().join("app").display().to_string()),
            concrete_paths: vec!["infrastructure".to_string()],
            instance_failures: InstanceFailures::Fail,
            ..Default::default()
        };
        evaluate_module(self.module.path(), "app", Some(&options))
    }

    fn error(&self) -> TestResult<String> {
        match self.evaluate() {
            Ok(result) => Err(format!("expected an error, got {:?}", result.instances).into()),
            Err(error) => Ok(error.to_string()),
        }
    }
}

const STATE: &str = "state: turso: url: \"libsql://db.turso.io\"";
const PROVIDER: &str = "providers: random: {source: \"hashicorp/random\", version: \"3.9.1\"}";

fn infrastructure(lines: &[&str]) -> String {
    format!("infrastructure: {{\n\t{}\n}}", lines.join("\n\t"))
}

#[test]
fn valid_infrastructure_evaluates_and_deserializes() -> TestResult {
    let fixture = SchemaFixture::new(&infrastructure(&[
        STATE,
        PROVIDER,
        "resources: pet: {type: \"random_pet\", configuration: length: 2}",
        "resources: id: {type: \"random_id\", provider: \"random\", dependsOn: [\"pet\"]}",
    ]))?;
    let result = fixture.evaluate()?;
    let instance = result.instances.get("app").ok_or("app instance missing")?;
    let project: Project = serde_json::from_value(instance.clone())?;
    let declared: cuenv_core::manifest::Infrastructure =
        serde_json::from_value(project.infrastructure.ok_or("infrastructure missing")?)?;
    assert_eq!(declared.resources["id"].depends_on, vec!["pet".to_string()]);
    assert_eq!(
        declared
            .state
            .turso
            .authentication_token_environment_variable,
        "TURSO_AUTH_TOKEN"
    );
    Ok(())
}

#[test]
fn misspelled_fields_are_rejected_at_every_level() -> TestResult {
    // `schema.#Project` embedded at file level opens the project struct; the
    // nested definitions must stay closed anyway. `resource:` for
    // `resources:` would otherwise read as "no resources" and plan the
    // deletion of everything.
    let cases = [
        (
            "resource",
            infrastructure(&[STATE, PROVIDER, "resource: pet: type: \"random_pet\""]),
        ),
        (
            "tursoo",
            infrastructure(&["state: {turso: url: \"libsql://db.turso.io\", tursoo: {}}"]),
        ),
        (
            "authTokenEnv",
            infrastructure(&[
                "state: turso: {url: \"libsql://db.turso.io\", authTokenEnv: \"TOKEN\"}",
            ]),
        ),
        (
            "sourcee",
            infrastructure(&[
                STATE,
                "providers: random: {source: \"hashicorp/random\", version: \"3.9.1\", sourcee: \"x\"}",
            ]),
        ),
        (
            "versions",
            infrastructure(&[
                STATE,
                "providers: random: {source: \"hashicorp/random\", path: \"/bin/provider\", versions: \"3.9.1\"}",
            ]),
        ),
        (
            "configurations",
            infrastructure(&[
                STATE,
                PROVIDER,
                "resources: pet: {type: \"random_pet\", configurations: {}}",
            ]),
        ),
    ];
    for (field, body) in cases {
        let message = SchemaFixture::new(&body)?.error()?;
        assert!(
            message.contains(&format!("{field}: field not allowed")),
            "{field}: {message}"
        );
        assert!(message.contains("app/env.cue:"), "{field}: {message}");
    }
    Ok(())
}

#[test]
fn dependency_cycles_are_left_to_cuenv() -> TestResult {
    let fixture = SchemaFixture::new(&infrastructure(&[
        STATE,
        PROVIDER,
        "resources: a: {type: \"random_pet\", dependsOn: [\"b\"]}",
        "resources: b: {type: \"random_pet\", dependsOn: [\"a\"]}",
    ]))?;
    fixture.evaluate()?;
    Ok(())
}

#[test]
fn registry_source_may_name_a_host_with_a_port() -> TestResult {
    SchemaFixture::new(&infrastructure(&[
        STATE,
        "providers: random: {source: \"registry.example.com:8443/hashicorp/random\", version: \"3.9.1\"}",
    ]))?
    .evaluate()?;
    Ok(())
}

const DEVELOPMENT: &str = "environments: dev: {providers: random: {source: \"hashicorp/random\", version: \"3.9.1\"}, resources: pet: type: \"random_pet\"}";

#[test]
fn undeclared_references_name_what_is_missing() -> TestResult {
    let fixture = SchemaFixture::new(&infrastructure(&[
        STATE,
        PROVIDER,
        "resources: pet: {type: \"random_pet\", dependsOn: [\"nosuch\"]}",
        "resources: other: {type: \"random_pet\", provider: \"missing\"}",
        "resources: server: {type: \"aws_instance\"}",
    ]))?;
    let message = fixture.error()?;
    for fragment in [
        "infrastructure._unresolved.\"resources.pet.dependsOn[0]\": no resource named \"nosuch\" in `resources`",
        "infrastructure._unresolved.\"resources.other.provider\": no provider named \"missing\" in `providers`",
        "infrastructure._unresolved.\"resources.server.type\": no provider named \"aws\" (the prefix of type \"aws_instance\")",
        "schema/infrastructure.cue:",
    ] {
        assert!(message.contains(fragment), "{fragment} missing: {message}");
    }
    Ok(())
}

#[test]
fn undeclared_references_in_an_environment_name_the_environment() -> TestResult {
    // The same checks apply to every `environments.NAME` configuration, and
    // an environment is checked against its own providers and resources only:
    // the top-level `random` provider and `pet` resource do not satisfy it.
    let fixture = SchemaFixture::new(&infrastructure(&[
        STATE,
        PROVIDER,
        "resources: pet: type: \"random_pet\"",
        "environments: dev: {",
        "\tresources: id: {type: \"random_id\", dependsOn: [\"pet\"]}",
        "\tresources: other: {type: \"random_pet\", provider: \"missing\"}",
        "}",
    ]))?;
    let message = fixture.error()?;
    for fragment in [
        "infrastructure.environments.dev._unresolved.\"resources.id.dependsOn[0]\": no resource named \"pet\"",
        "infrastructure.environments.dev._unresolved.\"resources.id.type\": no provider named \"random\" (the prefix of type \"random_id\")",
        "infrastructure.environments.dev._unresolved.\"resources.other.provider\": no provider named \"missing\"",
    ] {
        assert!(message.contains(fragment), "{fragment} missing: {message}");
    }
    assert!(
        !message.contains("infrastructure._unresolved"),
        "the valid top level must not report: {message}"
    );
    Ok(())
}

#[test]
fn an_environment_that_is_not_selected_is_still_checked() -> TestResult {
    let fixture = SchemaFixture::new(&infrastructure(&[
        STATE,
        DEVELOPMENT,
        "environments: prod: {providers: random: {source: \"hashicorp/random\"}}",
    ]))?;
    let message = fixture.error()?;
    assert!(
        message.contains("infrastructure.environments.prod.providers.random._versionOrPath"),
        "{message}"
    );
    assert!(!message.contains("environments.dev"), "{message}");
    Ok(())
}

#[test]
fn valid_environments_evaluate_and_select() -> TestResult {
    let fixture = SchemaFixture::new(&infrastructure(&[
        STATE,
        PROVIDER,
        "resources: top: type: \"random_pet\"",
        DEVELOPMENT,
        "environments: staging: {providerEnvironment: \"isolated\", providers: random: {source: \"hashicorp/random\", path: \"/bin/provider\"}}",
    ]))?;
    let result = fixture.evaluate()?;
    let instance = result.instances.get("app").ok_or("app instance missing")?;
    let project: Project = serde_json::from_value(instance.clone())?;
    let raw = project.infrastructure.ok_or("infrastructure missing")?;

    let top = Infrastructure::select(raw.clone(), None)?;
    assert!(top.resources.contains_key("top"));
    assert_eq!(top.provider_environment, ProviderEnvironment::Inherit);

    let dev = Infrastructure::select(raw.clone(), Some("dev"))?;
    assert_eq!(dev.resources.keys().collect::<Vec<_>>(), vec!["pet"]);
    assert_eq!(dev.provider_environment, ProviderEnvironment::Inherit);

    let staging = Infrastructure::select(raw, Some("staging"))?;
    assert!(staging.resources.is_empty());
    assert_eq!(staging.provider_environment, ProviderEnvironment::Isolated);
    Ok(())
}

#[test]
fn provider_environment_accepts_only_known_values() -> TestResult {
    for value in ["inherit", "isolated"] {
        SchemaFixture::new(&infrastructure(&[
            STATE,
            &format!("providerEnvironment: \"{value}\""),
        ]))?
        .evaluate()?;
    }
    let message = SchemaFixture::new(&infrastructure(&[
        STATE,
        "environments: dev: providerEnvironment: \"open\"",
    ]))?
    .error()?;
    assert!(message.contains("providerEnvironment"), "{message}");
    Ok(())
}

#[test]
fn provider_needs_exactly_one_of_version_and_path() -> TestResult {
    let neither = SchemaFixture::new(&infrastructure(&[
        STATE,
        "providers: random: source: \"hashicorp/random\"",
    ]))?
    .error()?;
    assert!(
        neither.contains("set `version` (an exact registry release) or `path`"),
        "{neither}"
    );

    let both = SchemaFixture::new(&infrastructure(&[
        STATE,
        "providers: random: {source: \"hashicorp/random\", version: \"3.9.1\", path: \"/bin/provider\"}",
    ]))?
    .error()?;
    assert!(
        both.contains("set exactly one of `version` and `path`, not both"),
        "{both}"
    );

    let empty_path = SchemaFixture::new(&infrastructure(&[
        STATE,
        "providers: random: {source: \"hashicorp/random\", path: \"\"}",
    ]))?
    .error()?;
    assert!(empty_path.contains("providers.random.path"), "{empty_path}");

    // The same rule applies inside an environment.
    let in_environment = SchemaFixture::new(&infrastructure(&[
        STATE,
        "environments: dev: providers: random: {source: \"hashicorp/random\", version: \"3.9.1\", path: \"/bin/provider\"}",
    ]))?
    .error()?;
    assert!(
        in_environment.contains(
            "infrastructure.environments.dev.providers.random._versionOrPath: set exactly one of `version` and `path`"
        ),
        "{in_environment}"
    );
    let empty_in_environment = SchemaFixture::new(&infrastructure(&[
        STATE,
        "environments: dev: providers: random: {source: \"hashicorp/random\", path: \"\"}",
    ]))?
    .error()?;
    assert!(
        empty_in_environment.contains("environments.dev.providers.random.path"),
        "{empty_in_environment}"
    );
    Ok(())
}

#[test]
fn invalid_turso_url_is_reported_without_repeating_it() -> TestResult {
    for url in [
        "libsql://db.turso.io?authToken=secret-token-value",
        "https://user:secret-token-value@db.turso.io",
        "http://db.turso.io/secret-token-value",
        "http://127.0.0.1:99999/secret-token-value",
    ] {
        let message =
            SchemaFixture::new(&infrastructure(&[&format!("state: turso: url: \"{url}\"")]))?
                .error()?;
        assert!(
            message.contains("`url` must be libsql://"),
            "{url}: {message}"
        );
        assert!(!message.contains("secret-token-value"), "{url}: {message}");
    }
    Ok(())
}

#[test]
fn turso_url_contract() -> TestResult {
    // The schema side of the URL contract shared with the Rust parser in
    // cuenv-infrastructure (state/turso.rs): scheme and `localhost` in any
    // case; plaintext only for loopback hosts (localhost, dotted 127.x.y.z,
    // [::1], [::ffff:127.x.y.z]); port 1-65535; no credentials, query,
    // fragment or whitespace.
    let accepted = [
        "libsql://db-acme.turso.io",
        "LIBSQL://DB-ACME.TURSO.IO/",
        "https://db.turso.io/prefix/",
        "wss://db.turso.io",
        "https://10.0.0.1:8443",
        "https://[2001:db8::1]:8443/path",
        "ws://localhost:8080",
        "http://LOCALHOST",
        "http://127.0.0.1:8080/",
        "http://127.255.255.255:65535",
        "http://[::1]:8080",
        "http://[::ffff:127.0.0.1]:8080",
        "HTTP://127.0.0.1:1",
    ];
    let rejected = [
        "postgres://db.turso.io",
        "db.turso.io",
        "https://",
        "http://:8080",
        " libsql://db.turso.io",
        "libsql://db.turso.io\u{a0}",
        "libsql://db.turso.io/a b",
        "http://db.turso.io",
        "http://10.0.0.1:8080",
        "http://[2001:db8::1]:8080",
        "http://localhost.example.com",
        "http://127.0.0.1.example.com",
        "http://127.1",
        "http://127.0.0.256",
        "http://[::ffff:7f00:1]",
        "http://[0:0:0:0:0:0:0:1]",
        "http://127.0.0.1:0",
        "http://127.0.0.1:",
        "https://db.turso.io:65536",
        "https://-db.turso.io",
    ];
    // One instance per URL, so one rejection cannot hide another; the
    // default policy leaves rejected instances out of the result.
    let fixture = SchemaFixture::new(&infrastructure(&[STATE]))?;
    let urls: Vec<&str> = accepted.iter().chain(rejected.iter()).copied().collect();
    for (index, url) in urls.iter().enumerate() {
        let directory = fixture.module.path().join(format!("checks/url{index}"));
        fs::create_dir_all(&directory)?;
        fs::write(
            directory.join("values.cue"),
            format!(
                "package check\n\nimport \"github.com/cuenv/cuenv/schema\"\n\nturso: schema.#TursoState & {{url: {}}}\n",
                serde_json::to_string(url)?
            ),
        )?;
    }
    let options = ModuleEvalOptions {
        recursive: true,
        package_name: Some("check".to_string()),
        concrete_paths: vec!["turso".to_string()],
        ..Default::default()
    };
    let result = evaluate_module(fixture.module.path(), "check", Some(&options))?;
    for (index, url) in urls.iter().enumerate() {
        let valid = result.instances.contains_key(&format!("checks/url{index}"));
        assert_eq!(valid, index < accepted.len(), "{url:?}");
    }
    Ok(())
}

#[test]
fn turso_url_is_checked_wherever_the_state_is_declared() -> TestResult {
    // One state serves every environment, so a bad URL is one error at the
    // single `state`, never repeated per environment.
    let message = SchemaFixture::new(&infrastructure(&[
        "state: turso: url: \"http://db.turso.io\"",
        DEVELOPMENT,
    ]))?
    .error()?;
    assert_eq!(message.matches("_invalidUrl").count(), 1, "{message}");
    Ok(())
}

/// A project that embeds `schema.#Project` at file level (`file_level`) or
/// unifies with it (`schema.#Project & {…}`), with the given `env` body.
fn environment_fixture(file_level: bool, env_body: &str) -> TestResult<SchemaFixture> {
    let repository = repository_root()?;
    let module = temporary_module("cuengine-environment-schema-")?;
    copy_cue_files(&repository.join("cue.mod"), &module.path().join("cue.mod"))?;
    copy_cue_files(&repository.join("schema"), &module.path().join("schema"))?;
    let project = module.path().join("app");
    fs::create_dir_all(&project)?;
    let source = if file_level {
        format!(
            "package app\n\nimport \"github.com/cuenv/cuenv/schema\"\n\nschema.#Project\n\nname: \"schema-fixture\"\n\nenv: {{\n{env_body}\n}}\n"
        )
    } else {
        format!(
            "package app\n\nimport \"github.com/cuenv/cuenv/schema\"\n\nschema.#Project & {{\n\tname: \"schema-fixture\"\n\tenv: {{\n{env_body}\n\t}}\n}}\n"
        )
    };
    fs::write(project.join("env.cue"), source)?;
    Ok(SchemaFixture { module })
}

impl SchemaFixture {
    /// Evaluate the project and deserialize it as an ordinary command does.
    fn project(&self) -> TestResult<Project> {
        let options = ModuleEvalOptions {
            package_name: Some("app".to_string()),
            target_dir: Some(self.module.path().join("app").display().to_string()),
            instance_failures: InstanceFailures::Fail,
            ..Default::default()
        };
        let result = evaluate_module(self.module.path(), "app", Some(&options))?;
        let instance = result.instances.get("app").ok_or("app instance missing")?;
        Ok(serde_json::from_value(instance.clone())?)
    }
}

const POLICY_VARIABLES: &str = r#"
	PLAIN: "visible"
	GUARDED: {value: "x", policies: [{allowInfrastructure: ["plan", "state-list"]}]}
	TASKS_ONLY: {value: "y", policies: [{allowTasks: ["build"]}]}
	EXEC: {resolver: "exec", command: "echo", args: ["secret"]}
	GUARDED_EXEC: {value: {resolver: "exec", command: "echo", args: ["secret"]}, policies: [{allowInfrastructure: ["apply"]}, {allowTasks: ["build"]}]}
	ONEPASSWORD: {resolver: "onepassword", ref: "op://vault/item/field"}
	GUARDED_ONEPASSWORD: {value: {resolver: "onepassword", ref: "op://vault/item/field"}, policies: [{allowInfrastructure: ["plan"], allowExec: ["deploy"]}]}
"#;

#[test]
fn documented_policy_form_evaluates_and_deserializes() -> TestResult {
    // `{value: …, policies: […]}` next to plain values and secrets: the
    // #EnvironmentVariable disjunction must pick exactly one alternative,
    // with `schema.#Project` embedded at file level and unified with `&`.
    for file_level in [true, false] {
        let fixture = environment_fixture(file_level, POLICY_VARIABLES)?;
        let project = fixture.project()?;
        let base = project.env.ok_or("env missing")?.base;
        assert_eq!(base.len(), 7, "file_level={file_level}");
        let accessible = |name: &str, action: InfrastructurePolicyAction| {
            base[name].is_accessible_by_infrastructure(action)
        };
        assert!(accessible("PLAIN", InfrastructurePolicyAction::Apply));
        assert!(accessible("EXEC", InfrastructurePolicyAction::Apply));
        assert!(accessible(
            "ONEPASSWORD",
            InfrastructurePolicyAction::Destroy
        ));
        assert!(accessible("GUARDED", InfrastructurePolicyAction::Plan));
        assert!(accessible("GUARDED", InfrastructurePolicyAction::StateList));
        assert!(!accessible("GUARDED", InfrastructurePolicyAction::Apply));
        assert!(!accessible("TASKS_ONLY", InfrastructurePolicyAction::Plan));
        assert!(accessible(
            "GUARDED_EXEC",
            InfrastructurePolicyAction::Apply
        ));
        assert!(!accessible(
            "GUARDED_EXEC",
            InfrastructurePolicyAction::Plan
        ));
        assert!(accessible(
            "GUARDED_ONEPASSWORD",
            InfrastructurePolicyAction::Plan
        ));
        assert!(!accessible(
            "GUARDED_ONEPASSWORD",
            InfrastructurePolicyAction::Unlock
        ));
        assert!(matches!(base["EXEC"], EnvValue::Secret(_)));
        assert!(matches!(base["ONEPASSWORD"], EnvValue::Secret(_)));
        assert!(matches!(base["GUARDED"], EnvValue::WithPolicies(_)));
    }
    Ok(())
}

#[test]
fn policy_form_works_in_an_environment_overlay() -> TestResult {
    let fixture = environment_fixture(
        true,
        "\tenvironment: staging: TOKEN: {value: \"x\", policies: [{allowTasks: [\"deploy\"], allowInfrastructure: [\"plan\"]}]}",
    )?;
    let project = fixture.project()?;
    let overlays = project
        .env
        .ok_or("env missing")?
        .environment
        .ok_or("overlay missing")?;
    assert!(matches!(
        overlays["staging"]["TOKEN"],
        EnvValue::WithPolicies(_)
    ));
    Ok(())
}

#[test]
fn misspelled_policy_fields_and_actions_are_rejected_by_the_schema() -> TestResult {
    for (policy, fragment) in [
        ("allowInfrastucture: [\"plan\"]", "allowInfrastucture"),
        ("allowInfrastructure: [\"deploy\"]", "allowInfrastructure"),
        ("allowInfrastructure: [\"Plan\"]", "allowInfrastructure"),
        ("allowInfrastructure: [\"state\"]", "allowInfrastructure"),
    ] {
        let fixture = environment_fixture(
            true,
            &format!("\tTOKEN: {{value: \"x\", policies: [{{{policy}}}]}}"),
        )?;
        let message = match fixture.project() {
            Ok(_) => return Err(format!("{policy} must be rejected").into()),
            Err(error) => error.to_string(),
        };
        assert!(message.contains(fragment), "{policy}: {message}");
    }
    Ok(())
}

#[test]
fn every_infrastructure_action_name_is_accepted() -> TestResult {
    let names = [
        "plan",
        "apply",
        "destroy",
        "state-list",
        "state-remove",
        "state-recover",
        "state-adopt",
        "unlock",
    ];
    let list = names
        .iter()
        .map(|name| format!("\"{name}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let fixture = environment_fixture(
        true,
        &format!("\tTOKEN: {{value: \"x\", policies: [{{allowInfrastructure: [{list}]}}]}}"),
    )?;
    let project = fixture.project()?;
    let token = &project.env.ok_or("env missing")?.base["TOKEN"];
    assert!(token.is_accessible_by_infrastructure(InfrastructurePolicyAction::StateAdopt));
    Ok(())
}
