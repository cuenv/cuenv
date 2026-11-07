use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use cuenv_infrastructure::ResourceAddress;
use cuenv_infrastructure::references::{ConfigurationSelector, NativeReference, SourceAddress};
use cuenv_manifest::lockfile::LockedInfrastructureProvider;
use cuenv_manifest::manifest::Infrastructure;
use serde_json::json;

use super::changes::{Change, Changes, Segment};
use super::markdown::{self, Comparison, code, path_text};
use super::snapshot::{Selected, Snapshot};
use super::{DiffOptions, execute_diff};

type TestResult<Success = ()> = Result<Success, Box<dyn std::error::Error>>;

const COMPARISON: Comparison<'static> = Comparison {
    base: "origin/main",
    commit: "0123456789abcdef0123456789abcdef01234567",
    environment: None,
};

/// The selected configuration `value` declares, with a state backend.
fn infrastructure(mut value: serde_json::Value) -> Infrastructure {
    value["state"] = json!({"turso": {"url": "http://127.0.0.1:8080"}});
    Infrastructure::select(value, None).expect("a valid configuration")
}

fn snapshot(value: serde_json::Value, references: &[NativeReference]) -> Snapshot {
    Snapshot::of(&Selected {
        infrastructure: &infrastructure(value),
        references,
        pins: None,
    })
}

fn resource(resource_type: &str, name: &str) -> SourceAddress {
    SourceAddress::Resource(ResourceAddress::new(resource_type, name))
}

fn field(name: &str) -> ConfigurationSelector {
    ConfigurationSelector::Field(name.to_string())
}

/// The configuration of a pull request that adds an R2 bucket, points a
/// worker at it and rotates the worker's secret, and drops a DNS record.
fn base_configuration() -> Snapshot {
    snapshot(
        json!({
            "providers": {"cloudflare": {"source": "cloudflare/cloudflare", "version": "5.0.0"}},
            "resources": {
                "api": {
                    "type": "cloudflare_workers_script",
                    "configuration": {
                        "script_name": "api",
                        "secret_text": {"resolver": "onepassword", "ref": "op://ci/api/old"},
                        "bindings": [{"name": "ASSETS", "type": "r2_bucket", "bucket_name": "assets-old"}],
                    },
                },
                "legacy": {"type": "cloudflare_dns_record", "configuration": {"name": "legacy"}},
            },
        }),
        &[],
    )
}

fn head_configuration() -> Snapshot {
    snapshot(
        json!({
            "providers": {"cloudflare": {"source": "cloudflare/cloudflare", "version": "5.1.0"}},
            "resources": {
                "assets": {
                    "type": "cloudflare_r2_bucket",
                    "configuration": {"name": "assets", "location": "weur"},
                },
                "api": {
                    "type": "cloudflare_workers_script",
                    "dependsOn": ["assets"],
                    "configuration": {
                        "script_name": "api",
                        "secret_text": {"resolver": "onepassword", "ref": "op://ci/api/new"},
                        // Evaluation leaves an output reference's destination null.
                        "bindings": [{"name": "ASSETS", "type": "r2_bucket", "bucket_name": null}],
                    },
                },
            },
        }),
        &[NativeReference {
            consumer: resource("cloudflare_workers_script", "api"),
            destination: vec![
                field("bindings"),
                ConfigurationSelector::Index(0),
                field("bucket_name"),
            ],
            source: resource("cloudflare_r2_bucket", "assets"),
            attribute: "name".to_string(),
            computed: false,
        }],
    )
}

#[test]
fn renders_added_changed_and_removed_declarations_by_address() {
    let rendered = markdown::render(
        &Changes::between(&base_configuration(), &head_configuration()),
        &COMPARISON,
    );
    assert_eq!(
        rendered,
        "Infrastructure changes compared with `origin/main` (`0123456`). Providers: 1 changed; \
         Resources: 1 added, 1 changed, 1 removed.

#### Providers

- **changed** `cloudflare`
  - `version`: `\"5.0.0\"` → `\"5.1.0\"`

#### Resources

- **removed** `cloudflare_dns_record.legacy`
- **added** `cloudflare_r2_bucket.assets`
  - `configuration.location`: `\"weur\"`
  - `configuration.name`: `\"assets\"`
  - `provider`: `\"cloudflare\"`
- **changed** `cloudflare_workers_script.api`
  - `configuration.bindings[0].bucket_name`: `\"assets-old\"` → `(output of cloudflare_r2_bucket.assets: name)`
  - `configuration.secret_text`: `(secret onepassword: ref=\"op://ci/api/old\")` → `(secret onepassword: ref=\"op://ci/api/new\")`
  - `dependsOn`: added `[\"assets\"]`
"
    );
}

#[test]
fn says_so_plainly_when_nothing_changed() {
    let changes = Changes::between(&head_configuration(), &head_configuration());
    assert!(changes.is_empty());
    assert_eq!(
        markdown::render(
            &changes,
            &Comparison {
                environment: Some("staging"),
                ..COMPARISON
            }
        ),
        "No infrastructure changes compared with `origin/main` (`0123456`) in environment \
         `staging`.\n"
    );
}

#[test]
fn an_empty_base_adds_every_declaration() {
    let changes = Changes::between(&Snapshot::default(), &head_configuration());
    assert!(
        changes
            .providers
            .values()
            .chain(changes.resources.values())
            .all(|change| matches!(change, Change::Added(_)))
    );
    let rendered = markdown::render(&changes, &COMPARISON);
    assert!(
        rendered.contains("Providers: 1 added; Resources: 2 added."),
        "{rendered}"
    );
    assert!(
        rendered.contains("  - `source`: `\"cloudflare/cloudflare\"`\n"),
        "{rendered}"
    );
    assert!(
        rendered.contains(
            "  - `configuration.bindings[0].bucket_name`: `(output of cloudflare_r2_bucket.assets: name)`\n"
        ),
        "{rendered}"
    );
    assert!(
        rendered.contains(
            "  - `configuration.secret_text`: `(secret onepassword: ref=\"op://ci/api/new\")`\n"
        ),
        "{rendered}"
    );
}

#[test]
fn secrets_show_what_they_name_and_never_a_value() {
    let configuration = |command: &str| {
        snapshot(
            json!({"resources": {"token": {"type": "random_password", "configuration": {
                "value_wo": {"resolver": "exec", "command": command, "args": ["--field", "token"], "writeOnly": true},
                "keeper": {"resolver": "infisical", "projectId": "p", "environment": "prod", "secretName": "API_TOKEN"},
            }}}}),
            &[],
        )
    };
    let rendered = markdown::render(
        &Changes::between(&configuration("print-old"), &configuration("print-new")),
        &COMPARISON,
    );
    assert!(
        rendered.contains(
            "  - `configuration.value_wo`: `(write-only secret exec: args=[\"--field\",\"token\"], \
             command=\"print-old\")` → `(write-only secret exec: args=[\"--field\",\"token\"], \
             command=\"print-new\")`\n"
        ),
        "{rendered}"
    );
    // The unchanged secret is not listed, and no secret's value exists to show.
    assert!(!rendered.contains("keeper"), "{rendered}");
    let added = markdown::render(
        &Changes::between(&Snapshot::default(), &configuration("print-old")),
        &COMPARISON,
    );
    assert!(
        added.contains(
            "  - `configuration.keeper`: `(secret infisical: environment=\"prod\", \
             projectId=\"p\", secretName=\"API_TOKEN\")`\n"
        ),
        "{added}"
    );
}

#[test]
fn an_object_with_another_resolver_is_ordinary_configuration() {
    let rendered = markdown::render(
        &Changes::between(
            &Snapshot::default(),
            &snapshot(
                json!({"resources": {"zone": {"type": "dns_zone", "configuration": {
                    "upstream": {"resolver": "1.1.1.1"},
                }}}}),
                &[],
            ),
        ),
        &COMPARISON,
    );
    assert!(
        rendered.contains("  - `configuration.upstream.resolver`: `\"1.1.1.1\"`\n"),
        "{rendered}"
    );
}

#[test]
fn computed_values_name_every_output_they_read() {
    let reference = |source: &str, attribute: &str| NativeReference {
        consumer: SourceAddress::DataSource(ResourceAddress::new("cloudflare_zone", "primary")),
        destination: vec![field("name")],
        source: resource("cloudflare_r2_bucket", source),
        attribute: attribute.to_string(),
        computed: true,
    };
    let head = snapshot(
        json!({"data": {"primary": {"type": "cloudflare_zone", "configuration": {"name": null}}}}),
        &[reference("assets", "name"), reference("logs", "id")],
    );
    let base = snapshot(
        json!({"data": {"primary": {"type": "cloudflare_zone", "configuration": {"name": "example.com"}}}}),
        &[],
    );
    let rendered = markdown::render(&Changes::between(&base, &head), &COMPARISON);
    assert!(rendered.contains("Data sources: 1 changed."), "{rendered}");
    assert!(
        rendered.contains(
            "- **changed** `data.cloudflare_zone.primary`\n  - `configuration.name`: \
             `\"example.com\"` → `(computed from outputs of cloudflare_r2_bucket.assets: name, \
             cloudflare_r2_bucket.logs: id)`\n"
        ),
        "{rendered}"
    );
    let single = snapshot(
        json!({"data": {"primary": {"type": "cloudflare_zone", "configuration": {"name": null}}}}),
        &[reference("assets", "name")],
    );
    let rendered = markdown::render(&Changes::between(&base, &single), &COMPARISON);
    assert!(
        rendered.contains("`(computed from output of cloudflare_r2_bucket.assets: name)`"),
        "{rendered}"
    );
}

#[test]
fn fields_removed_and_lists_of_scalars_change_as_a_whole() {
    let configuration = |value: serde_json::Value| {
        snapshot(
            json!({"resources": {"record": {"type": "cloudflare_dns_record", "configuration": value}}}),
            &[],
        )
    };
    let changes = Changes::between(
        &configuration(json!({"ttl": 300, "tags": ["a", "b"], "comment": "old"})),
        &configuration(json!({"ttl": 300, "tags": ["b", "a"]})),
    );
    let Some(Change::Changed(fields)) = changes.resources.get("cloudflare_dns_record.record")
    else {
        panic!("the record changed: {changes:?}");
    };
    let paths: Vec<String> = fields.iter().map(|field| path_text(&field.path)).collect();
    assert_eq!(paths, ["configuration.comment", "configuration.tags"]);
    let rendered = markdown::render(&changes, &COMPARISON);
    assert!(
        rendered.contains("  - `configuration.comment`: removed (was `\"old\"`)\n"),
        "{rendered}"
    );
    assert!(
        rendered.contains("  - `configuration.tags`: `[\"a\", \"b\"]` → `[\"b\", \"a\"]`\n"),
        "{rendered}"
    );
}

#[test]
fn the_provider_a_resource_uses_is_compared() {
    let configuration = |provider: Option<&str>| {
        let mut declaration = json!({"type": "cloudflare_dns_record"});
        if let Some(provider) = provider {
            declaration["provider"] = json!(provider);
        }
        snapshot(json!({"resources": {"record": declaration}}), &[])
    };
    // Naming the provider the type implies changes nothing.
    assert!(Changes::between(&configuration(None), &configuration(Some("cloudflare"))).is_empty());
    let rendered = markdown::render(
        &Changes::between(&configuration(None), &configuration(Some("cloudflare_eu"))),
        &COMPARISON,
    );
    assert!(
        rendered.contains("  - `provider`: `\"cloudflare\"` → `\"cloudflare_eu\"`\n"),
        "{rendered}"
    );
}

#[test]
fn lock_pins_decide_the_versions_of_borrowed_provider_types() {
    let digest = |character: char| format!("sha256:{}", character.to_string().repeat(64));
    let infrastructure = infrastructure(json!({"providers": {"cloudflare": {
        "source": "registry.terraform.io/cloudflare/cloudflare",
        "version": "5.1.0",
        "schemaDigest": digest('b'),
    }}}));
    let pins = BTreeMap::from([(
        "registry.terraform.io/cloudflare/cloudflare".to_string(),
        LockedInfrastructureProvider {
            version: "5.0.0".to_string(),
            schema_digest: digest('a'),
            platforms: BTreeMap::from([("linux_amd64".to_string(), digest('c'))]),
        },
    )]);
    let pinned = Snapshot::of(&Selected {
        infrastructure: &infrastructure,
        references: &[],
        pins: Some(&pins),
    });
    let declared = Snapshot::of(&Selected {
        infrastructure: &infrastructure,
        references: &[],
        pins: None,
    });
    let rendered = markdown::render(&Changes::between(&pinned, &declared), &COMPARISON);
    assert!(
        rendered.contains("  - `version`: `\"5.0.0\"` → `\"5.1.0\"`\n"),
        "{rendered}"
    );
    assert!(rendered.contains("  - `schemaDigest`: "), "{rendered}");
}

#[test]
fn paths_quote_keys_that_are_not_identifiers() {
    assert_eq!(
        path_text(&[
            Segment::Field("configuration".to_string()),
            Segment::Field("labels".to_string()),
            Segment::Field("app.kubernetes.io/name".to_string()),
            Segment::Index(2),
        ]),
        "configuration.labels.\"app.kubernetes.io/name\"[2]"
    );
}

#[test]
fn code_spans_hold_any_backticks() {
    assert_eq!(code("plain"), "`plain`");
    assert_eq!(code("a`b"), "``a`b``");
    assert_eq!(code("`edge"), "`` `edge ``");
    assert_eq!(code("a``b"), "```a``b```");
}

#[test]
fn long_values_are_shortened_with_their_length_and_digest() {
    let configuration = |script: String| {
        snapshot(
            json!({"resources": {"api": {"type": "cloudflare_workers_script", "configuration": {"content": script}}}}),
            &[],
        )
    };
    let rendered = markdown::render(
        &Changes::between(
            &configuration("x".repeat(500)),
            &configuration(format!("{}y", "x".repeat(499))),
        ),
        &COMPARISON,
    );
    let line = rendered
        .lines()
        .find(|line| line.contains("configuration.content"))
        .unwrap_or_default();
    assert_eq!(
        line.matches("(502 characters, sha256 `").count(),
        2,
        "{line}"
    );
    assert!(!line.contains(&"x".repeat(200)), "{line}");
}

// The command end to end: a git repository whose working tree changes a
// project's infrastructure.

/// A git repository holding a CUE module with cuenv's infrastructure schema.
struct Repository {
    directory: tempfile::TempDir,
}

impl Repository {
    fn new() -> TestResult<Self> {
        let directory = tempfile::tempdir()?;
        let repository = Self { directory };
        repository.git(&["init", "--quiet"])?;
        repository.git(&["config", "user.name", "Test User"])?;
        repository.git(&["config", "user.email", "test@example.com"])?;
        repository.write(
            "cue.mod/module.cue",
            "module: \"example.com/native\"\nlanguage: version: \"v0.14.1\"\n",
        )?;
        let schema = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../schema");
        for file in ["infrastructure.cue", "env.cue", "secrets.cue", "policy.cue"] {
            repository.write(
                &format!("schema/{file}"),
                &std::fs::read_to_string(schema.join(file))?,
            )?;
        }
        Ok(repository)
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.directory.path().join(relative)
    }

    fn write(&self, relative: &str, content: &str) -> TestResult {
        let path = self.path(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, content)?;
        Ok(())
    }

    fn git(&self, arguments: &[&str]) -> TestResult<String> {
        let output = Command::new("git")
            .current_dir(self.directory.path())
            .args(arguments)
            .output()?;
        if !output.status.success() {
            return Err(format!(
                "git {arguments:?} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )
            .into());
        }
        Ok(String::from_utf8(output.stdout)?)
    }

    fn commit(&self) -> TestResult {
        self.git(&["add", "--all"])?;
        self.git(&[
            "commit",
            "--quiet",
            "--no-gpg-sign",
            "--no-verify",
            "--allow-empty",
            "--message",
            "base",
        ])?;
        Ok(())
    }

    /// The worktrees git knows of; only the repository itself once a diff
    /// is done.
    fn worktrees(&self) -> TestResult<usize> {
        Ok(self
            .git(&["worktree", "list", "--porcelain"])?
            .lines()
            .filter(|line| line.starts_with("worktree "))
            .count())
    }

    async fn diff(&self, environment: Option<&str>) -> Result<String, crate::cli::CliError> {
        execute_diff(&DiffOptions {
            path: self.path("stack").to_string_lossy().into_owned(),
            package: "cuenv".to_string(),
            environment: environment.map(str::to_owned),
            base: "HEAD".to_string(),
        })
        .await
    }
}

/// A project named `stack` in `stack/env.cue` whose infrastructure is `body`.
fn project(body: &str) -> String {
    format!(
        r#"package cuenv
import "example.com/native/schema"
name: "stack"
infrastructure: schema.#Infrastructure & {{
 state: turso: url: "http://127.0.0.1:8080"
 providers: cloudflare: {{source: "cloudflare/cloudflare", version: "5.1.0"}}
{body}
}}
"#
    )
}

const BASE_RESOURCES: &str = r#" resources: {
  api: {type: "cloudflare_workers_script", configuration: {
   script_name: "api"
   secret_text: {resolver: "onepassword", ref: "op://ci/api/old"}
   bindings: [{name: "ASSETS", type: "r2_bucket", bucket_name: "assets-old"}]
  }}
  legacy: {type: "cloudflare_dns_record", configuration: name: "legacy"}
 }"#;

const HEAD_RESOURCES: &str = r#" resources: {
  assets: {type: "cloudflare_r2_bucket", configuration: name: "assets", outputs: name: string}
  api: {type: "cloudflare_workers_script", configuration: {
   script_name: "api"
   secret_text: {resolver: "onepassword", ref: "op://ci/api/new"}
   bindings: [{name: "ASSETS", type: "r2_bucket", bucket_name: infrastructure.resources.assets.outputs.name}]
  }}
 }"#;

#[tokio::test]
async fn diffs_the_working_tree_with_a_base_revision() -> TestResult {
    let repository = Repository::new()?;
    repository.write("stack/env.cue", &project(BASE_RESOURCES))?;
    repository.commit()?;
    repository.write("stack/env.cue", &project(HEAD_RESOURCES))?;
    let commit = repository.git(&["rev-parse", "--short=7", "HEAD"])?;
    let rendered = repository.diff(None).await?;
    assert_eq!(
        rendered,
        format!(
            "Infrastructure changes compared with `HEAD` (`{}`). Resources: 1 added, 1 changed, \
             1 removed.

#### Resources

- **removed** `cloudflare_dns_record.legacy`
- **added** `cloudflare_r2_bucket.assets`
  - `configuration.name`: `\"assets\"`
  - `provider`: `\"cloudflare\"`
- **changed** `cloudflare_workers_script.api`
  - `configuration.bindings[0].bucket_name`: `\"assets-old\"` → `(output of cloudflare_r2_bucket.assets: name)`
  - `configuration.secret_text`: `(secret onepassword: ref=\"op://ci/api/old\")` → `(secret onepassword: ref=\"op://ci/api/new\")`
",
            commit.trim()
        )
    );
    assert_eq!(repository.worktrees()?, 1);
    Ok(())
}

#[tokio::test]
async fn a_project_new_in_the_working_tree_is_all_added() -> TestResult {
    let repository = Repository::new()?;
    repository.commit()?;
    repository.write("stack/env.cue", &project(HEAD_RESOURCES))?;
    let rendered = repository.diff(None).await?;
    assert!(
        rendered.contains("Providers: 1 added; Resources: 2 added."),
        "{rendered}"
    );
    // An unchanged working tree has nothing to show.
    repository.commit()?;
    let rendered = repository.diff(None).await?;
    assert!(
        rendered.starts_with("No infrastructure changes compared with `HEAD`"),
        "{rendered}"
    );
    assert_eq!(repository.worktrees()?, 1);
    Ok(())
}

#[tokio::test]
async fn a_base_without_infrastructure_or_the_environment_is_empty() -> TestResult {
    let repository = Repository::new()?;
    repository.write("stack/env.cue", "package cuenv\nname: \"stack\"\n")?;
    repository.commit()?;
    repository.write("stack/env.cue", &project(HEAD_RESOURCES))?;
    let rendered = repository.diff(None).await?;
    assert!(rendered.contains("Resources: 2 added."), "{rendered}");

    repository.commit()?;
    repository.write(
        "stack/env.cue",
        &project(&format!(
            "{HEAD_RESOURCES}\n environments: staging: {{\n  providers: cloudflare: {{source: \
             \"cloudflare/cloudflare\", version: \"5.1.0\"}}\n  resources: zone: {{type: \
             \"cloudflare_zone\", configuration: name: \"staging.example.com\"}}\n }}"
        )),
    )?;
    let rendered = repository.diff(Some("staging")).await?;
    assert!(
        rendered.starts_with(&format!(
            "Infrastructure changes compared with `HEAD` (`{}`) in environment `staging`. \
             Providers: 1 added; Resources: 1 added.",
            repository.git(&["rev-parse", "--short=7", "HEAD"])?.trim()
        )),
        "{rendered}"
    );
    assert!(
        rendered.contains("- **added** `cloudflare_zone.zone`"),
        "{rendered}"
    );
    Ok(())
}

#[tokio::test]
async fn a_base_that_does_not_evaluate_is_an_error() -> TestResult {
    let repository = Repository::new()?;
    repository.write(
        "stack/env.cue",
        &project(
            " resources: broken: {type: \"cloudflare_dns_record\", configuration: name: string}",
        ),
    )?;
    repository.commit()?;
    repository.write("stack/env.cue", &project(HEAD_RESOURCES))?;
    assert!(repository.diff(None).await.is_err());
    assert_eq!(repository.worktrees()?, 1);
    Ok(())
}

#[tokio::test]
async fn an_unknown_base_revision_is_an_error() -> TestResult {
    let repository = Repository::new()?;
    repository.write("stack/env.cue", &project(HEAD_RESOURCES))?;
    repository.commit()?;
    let error = execute_diff(&DiffOptions {
        path: repository.path("stack").to_string_lossy().into_owned(),
        package: "cuenv".to_string(),
        environment: None,
        base: "origin/missing".to_string(),
    })
    .await
    .expect_err("no such revision");
    assert!(
        error
            .to_string()
            .contains("`origin/missing` is not a commit"),
        "{error}"
    );
    assert_eq!(repository.worktrees()?, 1);
    Ok(())
}

/// A provider whose types `cuenv infra init` generated into `cue.mod/gen`,
/// which git ignores, and pinned at `version` in `cuenv.lock`.
fn pin_generated_provider(repository: &Repository, version: &str, digest: char) -> TestResult {
    let schema_digest = format!("sha256:{}", digest.to_string().repeat(64));
    repository.write(
        "cue.mod/gen/registry.terraform.io/acme/thing/provider.cue",
        &format!(
            "package thing_provider\n#Provider: {{\n\tsource: \"registry.terraform.io/acme/thing\"\n\tversion: \"{version}\"\n\tschemaDigest: \"{schema_digest}\"\n}}\n"
        ),
    )?;
    repository.write(
        "cuenv.lock",
        &format!(
            "version = 5\n[infrastructure_providers.\"registry.terraform.io/acme/thing\"]\nversion = \"{version}\"\nschema_digest = \"{schema_digest}\"\n[infrastructure_providers.\"registry.terraform.io/acme/thing\".platforms]\nlinux_amd64 = \"sha256:{}\"\n",
            "c".repeat(64)
        ),
    )?;
    Ok(())
}

#[tokio::test]
async fn the_base_borrows_generated_types_and_reads_versions_from_its_lock() -> TestResult {
    let repository = Repository::new()?;
    repository.write(".gitignore", "cue.mod/gen/\n")?;
    let configuration = r#"package cuenv
import (
 "example.com/native/schema"
 "registry.terraform.io/acme/thing:thing_provider"
)
name: "stack"
infrastructure: schema.#Infrastructure & {
 state: turso: url: "http://127.0.0.1:8080"
 providers: thing: thing_provider.#Provider
 resources: widget: {type: "thing_widget", configuration: size: 2}
}
"#;
    repository.write("stack/env.cue", configuration)?;
    pin_generated_provider(&repository, "1.0.0", 'a')?;
    repository.commit()?;
    // The working tree moves the pin and regenerates the types.
    pin_generated_provider(&repository, "1.1.0", 'b')?;
    let rendered = repository.diff(None).await?;
    assert!(
        rendered.contains("- **changed** `thing`\n  - `schemaDigest`: "),
        "{rendered}"
    );
    assert!(
        rendered.contains("  - `version`: `\"1.0.0\"` → `\"1.1.0\"`\n"),
        "{rendered}"
    );
    assert!(!rendered.contains("thing_widget"), "{rendered}");
    assert_eq!(repository.worktrees()?, 1);
    Ok(())
}
