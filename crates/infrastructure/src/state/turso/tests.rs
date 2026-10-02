use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use super::*;

const RESOURCES_TABLE: &str = "cuenv_infrastructure_resources";
const LOCKS_TABLE: &str = "cuenv_infrastructure_locks";
const OWNERS_TABLE: &str = "cuenv_infrastructure_owners";
const PENDING_TABLE: &str = "cuenv_infrastructure_pending_migration";
/// The schema table of unreleased development builds.
const DEVELOPMENT_SCHEMA_TABLE: &str = "cuenv_infrastructure_schema";

fn configuration(url: &str) -> TursoConfiguration {
    TursoConfiguration {
        url: url.into(),
        authentication_token: Some("secret-token".into()),
    }
}

fn configuration_error(url: &str) -> String {
    match pipeline_url(url) {
        Err(InfrastructureError::Configuration(message)) => message,
        other => panic!("expected a configuration error for {url}, got {other:?}"),
    }
}

/// Drop every table cuenv created, so a test database can be reused.
async fn drop_state_tables(store: &TursoStateStore) {
    for table in [
        SCHEMA_TABLE,
        RESOURCES_TABLE,
        LOCKS_TABLE,
        OWNERS_TABLE,
        PENDING_TABLE,
        PROBE_TABLE,
        DEVELOPMENT_SCHEMA_TABLE,
        "cuenv_infrastructure_environment_resources",
    ] {
        store
            .execute(Statement::new(
                format!("DROP TABLE IF EXISTS {table}"),
                Vec::new(),
            ))
            .await
            .unwrap();
    }
}

/// A table a test migration creates, to see whether the migration ran.
const PROBE_TABLE: &str = "cuenv_infrastructure_probe";

/// The shipped migrations followed by a version that only creates
/// [`PROBE_TABLE`].
fn migrations_with_a_probe() -> [Migration; 2] {
    [
        Migration {
            version: MIGRATIONS[0].version,
            statements: MIGRATIONS[0].statements,
        },
        Migration {
            version: INITIAL_SCHEMA_VERSION + 1,
            statements: &["CREATE TABLE cuenv_infrastructure_probe (value INTEGER NOT NULL)"],
        },
    ]
}

async fn table_exists(store: &TursoStateStore, table: &str) -> bool {
    !store
        .execute(Statement::new(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?",
            vec![HranaValue::text(table)],
        ))
        .await
        .unwrap()
        .rows
        .is_empty()
}

fn probe_record() -> ManagedResource {
    ManagedResource {
        address: ResourceAddress::new("random_pet", "pet"),
        provider: "random".into(),
        provider_source: "registry.terraform.io/hashicorp/random".into(),
        schema_version: 0,
        state: json!({"id": "pet"}),
        private: Vec::new(),
        dependencies: Vec::new(),
        tainted: false,
        identity: None,
        serial: 0,
        generation: uuid::Uuid::nil(),
    }
}

/// The body of [`a_migration_refuses_while_any_lock_is_held`].
async fn fence_scenario(store: &TursoStateStore) -> Result<()> {
    let migrations = migrations_with_a_probe();
    store.migrate().await?;
    let tenant = TenantKey::new("example.com/migration", "web")?;
    let lock = store.lock(&tenant, "migration test").await?;

    // A run holds a lock: the migration refuses and changes nothing.
    let refused = store.migrate_to(&migrations).await.unwrap_err();
    // The error names the lock that blocks it, wherever it is held.
    let InfrastructureError::StateMigrationBlocked { version: 2, locks } = &refused else {
        panic!("{refused:?}");
    };
    assert_eq!(locks.len(), 1, "{locks:?}");
    assert_eq!(locks[0].lock.lock_identifier, lock.lock_identifier);
    assert_eq!(locks[0].project, "web");
    assert_eq!(locks[0].environment, None);
    assert!(
        refused.to_string().contains(&lock.lock_identifier),
        "{refused}"
    );
    // The same locks are listed, and released, without any project.
    assert_eq!(store.locks().await?, *locks);
    assert_eq!(store.schema_version().await?, INITIAL_SCHEMA_VERSION);
    assert!(
        !table_exists(store, PROBE_TABLE).await,
        "the refused migration must leave nothing behind"
    );
    // The run that holds the lock still writes into the shape it knows.
    store.put(&tenant, &lock, &probe_record()).await?;
    assert_eq!(store.list(&tenant).await?.len(), 1);

    // Several locks fence it too, and so does the lock of a tenant alone.
    let other = TenantKey::with_environment("example.com/migration", "web", "Dev")?;
    let other_lock = store.lock(&other, "migration test").await?;
    let both = store.migrate_to(&migrations).await.unwrap_err();
    assert!(
        matches!(&both, InfrastructureError::StateMigrationBlocked { version: 2, locks }
            if locks.len() == 2 && locks.iter().any(|held| held.environment.as_deref() == Some("Dev"))),
        "{both:?}"
    );
    // Releasing reports whether the lock was still held.
    assert!(store.unlock(&tenant, &lock).await?);
    assert!(!store.unlock(&tenant, &lock).await?);
    assert!(matches!(
        store.migrate_to(&migrations).await,
        Err(InfrastructureError::StateMigrationBlocked { version: 2, .. })
    ));
    // A lock of a project nothing evaluates is released by its tenant alone.
    let stale = store.locks().await?;
    assert_eq!(stale.len(), 1);
    assert!(
        store
            .force_unlock(&stale[0].tenant()?, &stale[0].lock.lock_identifier)
            .await?
    );
    assert!(store.locks().await?.is_empty());
    let _ = other_lock;

    // With no lock held it applies, and a client that does not know the
    // new version refuses to read or lock.
    let lock = store.lock(&tenant, "migration test").await?;
    store
        .delete(&tenant, &lock, &probe_record().address)
        .await?;
    store.unlock(&tenant, &lock).await?;
    store.migrate_to(&migrations).await?;
    assert_eq!(store.schema_version().await?, 2);
    assert!(table_exists(store, PROBE_TABLE).await);
    assert!(matches!(
        store.list(&tenant).await,
        Err(InfrastructureError::StateSchemaNewer {
            found: 2,
            supported: 1
        })
    ));
    assert!(matches!(
        store.lock(&tenant, "older client").await,
        Err(InfrastructureError::StateSchemaNewer { .. })
    ));
    Ok(())
}

/// Against a real server: a migration after the first refuses, inside its
/// transaction, while any lock row exists. Needs an empty database nothing
/// else uses; the test drops cuenv's tables at the end so it can run again.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires an empty isolated libSQL database (CUENV_INFRASTRUCTURE_TEST_TURSO_MIGRATION_URL)"]
async fn a_migration_refuses_while_any_lock_is_held() {
    let url = std::env::var("CUENV_INFRASTRUCTURE_TEST_TURSO_MIGRATION_URL").unwrap();
    let mut store = TursoStateStore::new(TursoConfiguration {
        url,
        authentication_token: std::env::var("TURSO_AUTH_TOKEN").ok(),
    })
    .unwrap();
    store.migration_wait = short_migration_wait();
    assert_eq!(
        store.stored_version().await.unwrap(),
        0,
        "the database must start without cuenv tables"
    );
    // Run in a task so the tables are dropped even if an assertion panics.
    let store = Arc::new(store);
    let scenario = tokio::spawn({
        let store = Arc::clone(&store);
        async move { fence_scenario(&store).await }
    });
    let outcome = scenario.await;
    drop_state_tables(&store).await;
    outcome.unwrap().unwrap();
}

#[test]
fn pipeline_url_normalizes_schemes() {
    let cases = [
        (
            "libsql://db-acme.turso.io",
            "https://db-acme.turso.io/v2/pipeline",
        ),
        (
            "LIBSQL://db-acme.turso.io/",
            "https://db-acme.turso.io/v2/pipeline",
        ),
        (
            "https://db.turso.io/prefix/",
            "https://db.turso.io/prefix/v2/pipeline",
        ),
        ("wss://db.turso.io", "https://db.turso.io/v2/pipeline"),
        ("ws://localhost:8080", "http://localhost:8080/v2/pipeline"),
        (
            "http://127.0.0.1:8080/",
            "http://127.0.0.1:8080/v2/pipeline",
        ),
    ];
    for (input, expected) in cases {
        assert_eq!(pipeline_url(input).unwrap().as_str(), expected, "{input}");
    }
    assert!(configuration_error("postgres://nope").contains("unsupported"));
    assert!(configuration_error("db.turso.io").contains("expected libsql://"));
}

/// URL validation builds no store and no HTTP client, so it also works where
/// the platform has no root certificates (a build sandbox), and it refuses
/// what creating a store refuses.
#[test]
fn a_url_is_validated_without_creating_a_store() {
    for url in [
        "libsql://db-acme.turso.io",
        "https://db.turso.io/prefix/",
        "http://127.0.0.1:8080/",
        "ws://localhost:8080",
    ] {
        assert!(TursoStateStore::validate_url(url).is_ok(), "{url}");
    }
    for url in [
        "http://db.turso.io",
        "https://user:secret@db.turso.io",
        "postgres://nope",
        "db.turso.io",
        "libsql:// db.turso.io",
    ] {
        let error = TursoStateStore::validate_url(url).unwrap_err();
        assert!(
            matches!(error, InfrastructureError::Configuration(_)),
            "{url}: {error}"
        );
        assert!(!error.to_string().contains("secret"), "{url}: {error}");
    }
}

/// The runtime URL contract. The CUE schema checks field shape and type;
/// this parser enforces the URL and transport rules.
#[test]
fn urls_follow_the_schema_contract() {
    let accepted = [
        "libsql://db-acme.turso.io",
        "LIBSQL://DB-ACME.TURSO.IO/",
        "https://db.turso.io/prefix/",
        "wss://db.turso.io",
        "https://10.0.0.1:8443",
        "https://1.2.3.4",
        "https://127.0.0.1:8443",
        "https://db1.example.com",
        "https://1password.example.com",
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
        "http://0177.0.0.1",
        "http://127.000.0.1",
        "http://[::ffff:7f00:1]",
        "http://[0:0:0:0:0:0:0:1]",
        "http://127.0.0.1:0",
        "http://127.0.0.1:",
        "https://db.turso.io:65536",
        "https://-db.turso.io",
        // A name that ends in a number is an address, and must be a dotted
        // decimal one; encrypted transport does not make the others valid.
        "https://0177.0.0.1",
        "https://127.000.0.1",
        "https://999.1.1.1",
        "https://256.0.0.1",
        "https://1.2.3.4.5",
        "https://1.2.3",
        "https://db.0x1",
        "https://[1:2:3]",
        "https://[:::]",
        "https://[12345::1]",
    ];
    for url in accepted {
        assert!(pipeline_url(url).is_ok(), "{url:?} must be accepted");
    }
    for url in rejected {
        configuration_error(url);
    }
}

#[test]
fn host_spellings_beyond_the_contract_are_rejected() {
    for url in [
        // Numeric forms the URL parser would read as loopback.
        "http://0x7f.1",
        "http://2130706433",
        "http://127.00.0.1",
        "http://[0:0:0:0:0:0:0:1]:8080",
        // International names, `_` and percent-encoding.
        "https://dé.turso.io",
        "https://db_1.turso.io",
        "https://d%62.turso.io",
        // Ports with leading zeros or no digits; bad path characters.
        "https://db.turso.io:08443",
        "https://db.turso.io:x",
        "https://db.turso.io/a\"b",
        "https://db.turso.io/a<b",
        // Whitespace anywhere, trimmed or not.
        "libsql://db.turso.io\n",
        "libsql://db.turso.io\t/x",
    ] {
        configuration_error(url);
    }
    for url in [
        "http://127.0.0.1",
        "http://[::FFFF:127.0.0.1]",
        "http://LocalHost:1",
    ] {
        assert_eq!(pipeline_url(url).unwrap().scheme(), "http", "{url}");
    }
    assert_eq!(
        pipeline_url("https://db.turso.io/a%20b;c=d@e")
            .unwrap()
            .path(),
        "/a%20b;c=d@e/v2/pipeline"
    );
}

#[test]
fn plaintext_urls_are_allowed_only_for_loopback_hosts() {
    for url in [
        "http://localhost",
        "http://LOCALHOST:8080",
        "http://127.0.0.1:8080",
        "http://127.1.2.3",
        "http://[::1]:8080",
        "http://[::ffff:127.0.0.1]:8080",
        "ws://127.0.0.1:8080",
    ] {
        assert_eq!(pipeline_url(url).unwrap().scheme(), "http", "{url}");
    }
    for url in [
        "http://db.turso.io",
        "http://10.0.0.1:8080",
        "http://[2001:db8::1]:8080",
        "ws://db.turso.io",
        "http://localhost.example.com",
        "http://127.0.0.1.example.com",
    ] {
        let message = configuration_error(url);
        assert!(message.contains("plaintext"), "{url}: {message}");
        assert!(
            message.contains("libsql://, https:// or wss://"),
            "{message}"
        );
    }
}

#[test]
fn urls_without_host_or_with_embedded_secrets_are_rejected() {
    for url in ["https://", "libsql://", "http://:8080"] {
        let message = configuration_error(url);
        assert!(
            message.contains("host") || message.contains("invalid"),
            "{url}: {message}"
        );
    }
    for url in [
        "libsql://db.turso.io?authToken=secret-token",
        "https://user:secret-token@db.turso.io",
        "https://db.turso.io#secret-token",
    ] {
        let message = configuration_error(url);
        assert!(!message.contains("secret-token"), "{message}");
        assert!(
            message.contains("must not contain credentials"),
            "{message}"
        );
    }
}

#[test]
fn rejected_url_components_are_never_echoed() {
    for url in [
        "secret-token-value://db.turso.io",
        "http://secret-token-value",
        "https://db.turso.io:secret-token-value",
    ] {
        let message = configuration_error(url);
        assert!(
            !message.contains("secret-token-value"),
            "rejected URL component leaked: {message}"
        );
    }
}

#[test]
fn store_debug_redacts_token() {
    let store = TursoStateStore::new(configuration("libsql://db.turso.io")).unwrap();
    let rendered = format!("{store:?}");
    assert!(!rendered.contains("secret-token"), "{rendered}");
    assert!(rendered.contains("<redacted>"), "{rendered}");
    assert!(
        rendered.contains("https://db.turso.io/v2/pipeline"),
        "{rendered}"
    );
}

#[test]
fn configuration_debug_redacts_token() {
    let rendered = format!("{:?}", configuration("libsql://db.turso.io"));
    assert!(!rendered.contains("secret-token"));
    assert!(rendered.contains("<redacted>"));
}

#[test]
fn retry_policy_backs_off_exponentially() {
    let policy = RetryPolicy::DEFAULT;
    assert_eq!(policy.retries, 3);
    let delays: Vec<Duration> = (0..policy.retries)
        .map(|retry| policy.delay(retry))
        .collect();
    assert_eq!(delays, [200, 400, 800].map(Duration::from_millis).to_vec());
}

#[test]
fn classifies_http_statuses() {
    for status in [408, 429, 500, 502, 503, 504] {
        let status = reqwest::StatusCode::from_u16(status).unwrap();
        assert!(status_is_transient(status), "{status}");
    }
    for status in [400, 401, 403, 404, 409] {
        let status = reqwest::StatusCode::from_u16(status).unwrap();
        assert!(!status_is_transient(status), "{status}");
    }
}

#[test]
fn classifies_statement_errors() {
    let busy = HranaError {
        message: "database is locked".into(),
        code: Some("SQLITE_BUSY".into()),
    };
    assert!(statement_failure(&busy, Disclosure::Full).is_transient());
    let syntax = HranaError {
        message: "near \"SELEC\": syntax error".into(),
        code: Some("SQLITE_ERROR".into()),
    };
    let failure = statement_failure(&syntax, Disclosure::Full);
    assert_eq!(failure.kind, FailureKind::Statement);
    assert!(failure.message.contains("syntax error (SQLITE_ERROR)"));
}

#[test]
fn errors_about_statements_carrying_state_quote_only_codes() {
    let echo = HranaError {
        message: "invalid argument \"hunter2\"\u{1b}[2J".into(),
        code: Some("SQLITE_TOOBIG".into()),
    };
    let withheld = statement_failure(&echo, Disclosure::CodeOnly).message;
    assert!(withheld.contains("SQLITE_TOOBIG"), "{withheld}");
    assert!(!withheld.contains("hunter2"), "{withheld}");
    let quoted = statement_failure(&echo, Disclosure::Full).message;
    assert!(quoted.contains("hunter2"), "{quoted}");
    assert!(!quoted.contains('\u{1b}'), "{quoted}");

    let body = BodyPrefix {
        bytes: br#"{"message": "bad state_json \"hunter2\"", "code": "HTTP_BAD"}"#.to_vec(),
        truncated: false,
    };
    let described = describe_http_error(&HttpError {
        status: reqwest::StatusCode::BAD_REQUEST,
        body: &body,
        total_bytes: None,
        disclosure: Disclosure::CodeOnly,
    });
    assert!(described.contains("HTTP 400"), "{described}");
    assert!(described.contains("(HTTP_BAD)"), "{described}");
    assert!(!described.contains("hunter2"), "{described}");
}

#[test]
fn corrupt_rows_are_reported_without_their_content() {
    let mut row = vec![
        HranaValue::text("random_pet"),
        HranaValue::text("pet"),
        HranaValue::text("random"),
        HranaValue::text("registry.terraform.io/hashicorp/random"),
        HranaValue::integer(1),
        HranaValue::text("{\"password\": hunter2}"),
        HranaValue::Null,
        HranaValue::text("[]"),
        HranaValue::integer(0),
        HranaValue::Null,
        HranaValue::integer(1),
        HranaValue::text(&uuid::Uuid::from_u128(7).to_string()),
    ];
    let error = row_to_resource(&row).unwrap_err();
    assert!(
        matches!(&error, InfrastructureError::UndecodableRecord { address, .. } if address == "random_pet.pet"),
        "{error:?}"
    );
    let message = error.to_string();
    assert!(message.contains("state_json"), "{message}");
    assert!(message.contains("line 1"), "{message}");
    assert!(!message.contains("hunter2"), "{message}");
    row[5] = HranaValue::text("{}");
    row[6] = HranaValue::Blob {
        base64: "!!!!".into(),
    };
    let message = row_to_resource(&row).unwrap_err().to_string();
    assert!(message.contains("random_pet.pet"), "{message}");
    assert!(!message.contains('!'), "{message}");
}

#[test]
fn a_row_without_a_readable_address_still_gets_the_undecodable_error() {
    let error = row_to_resource(&[HranaValue::Null, HranaValue::text("pet")]).unwrap_err();
    assert!(
        matches!(&error, InfrastructureError::UndecodableRecord { .. }),
        "{error:?}"
    );
    assert!(error.to_string().contains("resource_type"), "{error}");
}

#[test]
fn describes_error_body_prefixes() {
    let complete = BodyPrefix {
        bytes: b"short".to_vec(),
        truncated: false,
    };
    assert_eq!(describe_error_body(&complete, Some(5)), "short");
    // A two-byte character cut in half at the limit is dropped.
    let mut bytes = "é".repeat(2048).into_bytes();
    bytes.truncate(MAXIMUM_ERROR_BODY_BYTES - 1);
    let cut = BodyPrefix {
        bytes,
        truncated: true,
    };
    let described = describe_error_body(&cut, Some(6000));
    let kept = described.split('…').next().unwrap();
    assert_eq!(kept, "é".repeat(2047));
    assert!(described.ends_with("(truncated, 6000 bytes in total)"));
    assert!(describe_error_body(&cut, None).ends_with("… (truncated)"));
}

#[test]
fn pipeline_body_matches_hrana_wire_format() {
    let statement = Statement::new(
        "SELECT ?",
        vec![
            HranaValue::text("a"),
            HranaValue::integer(7),
            HranaValue::blob(&[1, 2, 3]),
            HranaValue::blob(&[]),
            HranaValue::optional_text(None),
        ],
    );
    let body = PipelineBody {
        baton: None,
        requests: vec![
            PipelineRequest::Execute {
                statement: &statement,
            },
            PipelineRequest::Close,
        ],
    };
    assert_eq!(
        serde_json::to_value(&body).unwrap(),
        json!({
            "baton": null,
            "requests": [
                {"type": "execute", "stmt": {"sql": "SELECT ?", "want_rows": true, "args": [
                    {"type": "text", "value": "a"},
                    {"type": "integer", "value": "7"},
                    {"type": "blob", "base64": "AQID"},
                    {"type": "null"},
                    {"type": "null"},
                ]}},
                {"type": "close"},
            ],
        })
    );
}

#[test]
fn transaction_batch_matches_hrana_wire_format() {
    let statements = [
        Statement::new("BEGIN IMMEDIATE", Vec::new()),
        Statement::new("ALTER TABLE t ADD COLUMN c", Vec::new()),
        Statement::new("COMMIT", Vec::new()),
        Statement::new("ROLLBACK", Vec::new()),
    ];
    let body = PipelineBody {
        baton: None,
        requests: vec![PipelineRequest::Batch {
            batch: Batch {
                steps: transaction_steps(&statements, 2),
            },
        }],
    };
    let steps = serde_json::to_value(&body).unwrap()["requests"][0]["batch"]["steps"].clone();
    assert_eq!(steps[0].get("condition"), None);
    assert_eq!(steps[1]["condition"], json!({"type": "ok", "step": 0}));
    assert_eq!(steps[2]["condition"], json!({"type": "ok", "step": 1}));
    assert_eq!(
        steps[3]["condition"],
        json!({"type": "not", "cond": {"type": "ok", "step": 2}})
    );
    assert_eq!(steps[3]["stmt"]["sql"], "ROLLBACK");
}

#[test]
fn interprets_migration_batch_results() {
    let parse = |value: Value| -> BatchResult { serde_json::from_value(value).unwrap() };
    let execute = json!({"cols": [], "rows": [], "affected_row_count": 0});
    let first = MigrationTransaction::of(&MIGRATIONS[0]);
    let steps = first.statements.len();
    let committed = parse(json!({
        // Every step ran except the rollback.
        "step_results": (0..steps)
            .map(|index| if index == steps - 1 { Value::Null } else { execute.clone() })
            .collect::<Vec<_>>(),
        "step_errors": vec![Value::Null; steps],
    }));
    assert_eq!(
        migration_outcome(&committed, &first).unwrap(),
        MigrationOutcome::Applied
    );
    let failing = |message: &str| {
        let mut errors = vec![Value::Null; steps];
        errors[1] = json!({"message": message, "code": "SQLITE_ERROR"});
        parse(json!({
            "step_results": vec![Value::Null; steps],
            "step_errors": errors,
        }))
    };
    for message in [
        "SQLite error: duplicate column name: tainted",
        "SQLite error: table cuenv_infrastructure_resources already exists",
    ] {
        assert_eq!(
            migration_outcome(&failing(message), &first).unwrap(),
            MigrationOutcome::AlreadyApplied,
            "{message}"
        );
    }
    let failure = migration_outcome(&failing("no such table: t"), &first).unwrap_err();
    assert_eq!(failure.kind, FailureKind::Statement);
    assert!(failure.message.contains("no such table"));
}

#[test]
fn a_failing_fence_statement_means_a_lock_is_held_and_nothing_else_does() {
    let later = Migration {
        version: INITIAL_SCHEMA_VERSION + 1,
        statements: &["CREATE TABLE cuenv_infrastructure_probe (value INTEGER)"],
    };
    let transaction = MigrationTransaction::of(&later);
    let fence = transaction.fence_index.unwrap();
    let steps = transaction.statements.len();
    let errors_at = |index: usize, message: &str| -> BatchResult {
        let mut errors = vec![Value::Null; steps];
        errors[index] = json!({"message": message, "code": "SQLITE_CONSTRAINT_CHECK"});
        serde_json::from_value(json!({
            "step_results": vec![Value::Null; steps],
            "step_errors": errors,
        }))
        .unwrap()
    };
    // The fence trips with an integer overflow; any other failure of that
    // statement is an ordinary failure, not a held lock.
    assert_eq!(
        migration_outcome(
            &errors_at(fence, "SQLite error: integer overflow"),
            &transaction
        )
        .unwrap(),
        MigrationOutcome::LockHeld
    );
    let failure = migration_outcome(
        &errors_at(fence, "CHECK constraint failed: lock_rows = 0"),
        &transaction,
    )
    .unwrap_err();
    assert_eq!(failure.kind, FailureKind::Statement);
    // The same text from any other statement is an ordinary failure.
    let failure = migration_outcome(
        &errors_at(fence + 3, "SQLite error: integer overflow"),
        &transaction,
    )
    .unwrap_err();
    assert_eq!(failure.kind, FailureKind::Statement);
    // The fence runs after BEGIN and before any statement of the migration.
    let sql: Vec<&str> = transaction
        .statements
        .iter()
        .map(|statement| statement.sql.as_str())
        .collect();
    assert_eq!(sql[0], "BEGIN IMMEDIATE");
    assert!(sql[fence].contains("cuenv_infrastructure_locks"), "{sql:?}");
    let body = sql
        .iter()
        .position(|statement| statement.contains("cuenv_infrastructure_probe"))
        .unwrap();
    assert!(fence < body);
    // The first migration creates the lock table, so it has nothing to fence.
    assert_eq!(MigrationTransaction::of(&MIGRATIONS[0]).fence_index, None);
}

#[test]
fn migrations_are_ordered_and_start_at_one() {
    let versions: Vec<i64> = MIGRATIONS
        .iter()
        .map(|migration| migration.version)
        .collect();
    let expected: Vec<i64> = (1..=i64::try_from(MIGRATIONS.len()).unwrap()).collect();
    assert_eq!(versions, expected);
    assert_eq!(versions[0], INITIAL_SCHEMA_VERSION);
}

#[test]
fn schema_version_one_creates_one_table_family_keyed_by_environment() {
    let migration = &MIGRATIONS[0];
    assert_eq!(migration.version, 1);
    assert_eq!(migration.statements.len(), 4);
    assert!(migration.statements[3].starts_with(&format!("CREATE TABLE {PENDING_TABLE} (")));
    for (statement, table, key) in [
        (
            migration.statements[0],
            "cuenv_infrastructure_resources",
            "PRIMARY KEY (module_path, project, environment, resource_type, resource_name)",
        ),
        (
            migration.statements[1],
            "cuenv_infrastructure_locks",
            "PRIMARY KEY (module_path, project, environment)",
        ),
        (
            migration.statements[2],
            "cuenv_infrastructure_owners",
            "PRIMARY KEY (module_path, project, environment)",
        ),
    ] {
        // Plain CREATE: tables of an unreleased layout are not adopted.
        assert!(statement.starts_with(&format!("CREATE TABLE {table} (")));
        assert!(statement.contains("environment TEXT NOT NULL"));
        assert!(statement.contains(key));
        assert!(statement.ends_with("WITHOUT ROWID"));
    }
    let resources = migration.statements[0];
    assert!(resources.contains("generation TEXT NOT NULL,"));
    assert!(resources.contains("serial INTEGER NOT NULL DEFAULT 1"));
    // Nothing is altered, backfilled or dropped: no earlier layout is read.
    assert!(migration.statements.iter().all(|statement| {
        !statement.contains("ALTER TABLE")
            && !statement.contains("UPDATE ")
            && !statement.contains("DROP TABLE")
    }));
}

#[test]
fn parses_pipeline_results_and_rows() {
    let response: PipelineResponse = serde_json::from_value(json!({
        "baton": null,
        "base_url": null,
        "results": [
            {"type": "ok", "response": {"type": "execute", "result": {
                "cols": [{"name": "a", "decltype": "TEXT"}],
                "rows": [[
                    {"type": "text", "value": "random_pet"},
                    {"type": "integer", "value": "3"},
                    {"type": "blob", "base64": "AQID"},
                    {"type": "null"},
                ]],
                "affected_row_count": 0,
                "last_insert_rowid": null,
                "rows_read": 1,
            }}},
            {"type": "error", "error": {"message": "boom", "code": "SQLITE_ERROR"}},
            {"type": "ok", "response": {"type": "close"}},
        ],
    }))
    .unwrap();
    let PipelineResult::Ok {
        response: StreamResponse::Execute { result },
    } = &response.results[0]
    else {
        panic!("expected execute result");
    };
    let row = &result.rows[0];
    assert_eq!(row[0].as_text(), Some("random_pet"));
    assert_eq!(row[1].as_integer(), Some(3));
    assert_eq!(row[2].as_blob().unwrap(), Some(vec![1, 2, 3]));
    assert_eq!(row[3].as_blob().unwrap(), None);
    assert!(matches!(response.results[1], PipelineResult::Error { .. }));
    assert!(matches!(
        response.results[2],
        PipelineResult::Ok {
            response: StreamResponse::Other
        }
    ));
}

#[test]
fn row_to_resource_reads_tainted_and_identity() {
    let row = |tainted: &str, identity: HranaValue| {
        vec![
            HranaValue::text("random_pet"),
            HranaValue::text("pet"),
            HranaValue::text("random"),
            HranaValue::text("registry.terraform.io/hashicorp/random"),
            HranaValue::integer(1),
            HranaValue::text("{\"id\":\"x\"}"),
            HranaValue::Null,
            HranaValue::text("[]"),
            HranaValue::Integer {
                value: tainted.into(),
            },
            identity,
            HranaValue::integer(7),
            HranaValue::text(&uuid::Uuid::from_u128(7).to_string()),
        ]
    };
    let resource = row_to_resource(&row("1", HranaValue::text("{\"name\":\"a\"}"))).unwrap();
    assert!(resource.tainted);
    assert_eq!(resource.serial, 7);
    assert_eq!(resource.identity, Some(json!({"name": "a"})));
    let resource = row_to_resource(&row("0", HranaValue::Null)).unwrap();
    assert!(!resource.tainted);
    assert_eq!(resource.identity, None);
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires a libSQL server (CUENV_INFRASTRUCTURE_TEST_TURSO_URL)"]
async fn concurrent_migrations_converge() {
    let url = std::env::var("CUENV_INFRASTRUCTURE_TEST_TURSO_URL").unwrap();
    let store = Arc::new(
        TursoStateStore::new(TursoConfiguration {
            url,
            authentication_token: std::env::var("TURSO_AUTH_TOKEN").ok(),
        })
        .unwrap(),
    );
    let mut migrators = tokio::task::JoinSet::new();
    for _ in 0..8 {
        let store = Arc::clone(&store);
        migrators.spawn(async move { store.migrate().await });
    }
    while let Some(outcome) = migrators.join_next().await {
        outcome.unwrap().unwrap();
    }
    let latest = MIGRATIONS.last().unwrap().version;
    assert_eq!(store.schema_version().await.unwrap(), latest);
    store.migrate().await.unwrap();
    assert_eq!(store.schema_version().await.unwrap(), latest);
}

/// Against a real server: reads on a database cuenv never touched are
/// empty and create nothing, and a schema newer than this build is
/// refused everywhere. The database must be one nothing else uses; the
/// test drops cuenv's tables at the end so it can run again.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires an otherwise unused libSQL database (CUENV_INFRASTRUCTURE_TEST_FRESH_TURSO_URL)"]
async fn fresh_database_reads_and_newer_schema_against_a_server() {
    let url = std::env::var("CUENV_INFRASTRUCTURE_TEST_FRESH_TURSO_URL").unwrap();
    let store = TursoStateStore::new(TursoConfiguration {
        url,
        authentication_token: std::env::var("TURSO_AUTH_TOKEN").ok(),
    })
    .unwrap();
    assert_eq!(
        store.stored_version().await.unwrap(),
        0,
        "the database must start without cuenv tables"
    );
    assert!(!table_exists(&store, SCHEMA_TABLE).await);

    assert_eq!(store.list(&tenant()).await.unwrap(), Vec::new());
    assert_eq!(store.current_lock(&tenant()).await.unwrap(), None);
    assert_eq!(store.owner(&tenant()).await.unwrap(), None);
    assert!(!store.force_unlock(&tenant(), "any").await.unwrap());
    assert!(store.lock(&tenant(), "test").await.is_err());
    assert!(
        !table_exists(&store, SCHEMA_TABLE).await,
        "reads and a refused lock create nothing"
    );

    store.migrate().await.unwrap();
    let lock = store.lock(&tenant(), "test").await.unwrap();
    store.unlock(&tenant(), &lock).await.unwrap();

    store
        .execute(Statement::new(
            "INSERT INTO cuenv_infrastructure_migrations (version) VALUES (?)",
            vec![HranaValue::integer(LATEST_SCHEMA_VERSION + 1)],
        ))
        .await
        .unwrap();
    let refusals = [
        store.migrate().await.err(),
        store.list(&tenant()).await.err(),
        store.current_lock(&tenant()).await.err(),
        store.lock(&tenant(), "test").await.err(),
        store.force_unlock(&tenant(), "any").await.err(),
        store.owner(&tenant()).await.err(),
    ];
    drop_state_tables(&store).await;
    for refusal in refusals {
        let error = refusal.expect("a newer schema must be refused");
        assert!(
            matches!(error, InfrastructureError::StateSchemaNewer { .. }),
            "{error:?}"
        );
    }
}

// -----------------------------------------------------------------------
// A minimal HTTP server standing in for Turso.
// -----------------------------------------------------------------------

/// How the fake server answers one request: `None` drops the connection.
type Responder = dyn Fn(usize, Value) -> Option<(u16, String)> + Send + Sync;

struct FakeServer {
    url: String,
    requests: Arc<AtomicUsize>,
}

async fn fake_server(responder: Arc<Responder>) -> FakeServer {
    fake_server_with_headers(responder, String::new()).await
}

/// A fake server whose every reply also carries `headers` (each line
/// ending in `\r\n`).
async fn fake_server_with_headers(responder: Arc<Responder>, headers: String) -> FakeServer {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&requests);
    let replies = Arc::new(Replies { responder, headers });
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let index = counter.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(answer(stream, index, Arc::clone(&replies)));
        }
    });
    FakeServer { url, requests }
}

/// How a fake server replies.
struct Replies {
    responder: Arc<Responder>,
    headers: String,
}

async fn answer(mut stream: TcpStream, index: usize, replies: Arc<Replies>) {
    let Replies { responder, headers } = replies.as_ref();
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 4096];
    let header_end = loop {
        let read = stream.read(&mut chunk).await.unwrap();
        if read == 0 {
            return;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(position) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break position + 4;
        }
    };
    let request_headers = String::from_utf8_lossy(&buffer[..header_end]).to_ascii_lowercase();
    let length: usize = request_headers
        .lines()
        .find_map(|line| line.strip_prefix("content-length:"))
        .map_or(0, |value| value.trim().parse().unwrap());
    while buffer.len() < header_end + length {
        let read = stream.read(&mut chunk).await.unwrap();
        if read == 0 {
            return;
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
    let body: Value = serde_json::from_slice(&buffer[header_end..header_end + length]).unwrap();
    let Some((status, response)) = responder(index, body) else {
        return;
    };
    let reply = format!(
        "HTTP/1.1 {status} Status\r\ncontent-type: application/json\r\n{headers}\
         content-length: {}\r\nconnection: close\r\n\r\n{response}",
        response.len()
    );
    stream.write_all(reply.as_bytes()).await.unwrap();
    stream.shutdown().await.unwrap();
}

fn execute_response(rows: &Value, affected_row_count: u64) -> String {
    json!({
        "baton": null,
        "base_url": null,
        "results": [
            {"type": "ok", "response": {"type": "execute", "result": {
                "cols": [], "rows": rows, "affected_row_count": affected_row_count,
            }}},
            {"type": "ok", "response": {"type": "close"}},
        ],
    })
    .to_string()
}

/// Answer every statement of a pipeline with `rows_for(sql)`; `None` is
/// a statement error.
fn pipeline_response(body: &Value, rows_for: impl Fn(&str) -> Option<Value>) -> String {
    let results: Vec<Value> = body["requests"]
        .as_array()
        .unwrap()
        .iter()
        .map(|request| match request["type"].as_str().unwrap() {
            "execute" => {
                let sql = request["stmt"]["sql"].as_str().unwrap();
                rows_for(sql).map_or_else(
                    || {
                        json!({"type": "error", "error": {
                            "message": format!("unexpected statement: {sql}"),
                            "code": "SQLITE_ERROR",
                        }})
                    },
                    |rows| {
                        json!({"type": "ok", "response": {"type": "execute", "result": {
                            "cols": [], "rows": rows, "affected_row_count": 1,
                        }}})
                    },
                )
            }
            _ => json!({"type": "ok", "response": {"type": "close"}}),
        })
        .collect();
    json!({"baton": null, "base_url": null, "results": results}).to_string()
}

/// Rows a database at schema `version` returns for the store's schema
/// inspection; `None` for any other statement.
fn schema_rows(sql: &str, version: i64) -> Option<Value> {
    if sql.starts_with("SELECT name FROM sqlite_master") {
        Some(json!([[{"type": "text", "value": SCHEMA_TABLE}]]))
    } else if sql == SELECT_SCHEMA_VERSION {
        Some(json!([[{"type": "integer", "value": version.to_string()}]]))
    } else {
        None
    }
}

fn first_statement(body: &Value) -> String {
    body["requests"][0]["stmt"]["sql"]
        .as_str()
        .unwrap()
        .to_string()
}

fn fast_store(url: &str, timeout: Duration) -> TursoStateStore {
    let mut store = TursoStateStore::with_timeout(configuration(url), timeout).unwrap();
    store.retry_policy = RetryPolicy {
        retries: 3,
        initial_delay: Duration::from_millis(1),
    };
    store
}

fn tenant() -> TenantKey {
    TenantKey::new("example.com/fake", "web").unwrap()
}

#[tokio::test]
async fn retries_transient_http_statuses_then_succeeds() {
    let server = fake_server(Arc::new(|index, _| {
        Some(match index {
            0 => (503, "unavailable".to_string()),
            1 => (429, "slow down".to_string()),
            _ => (200, execute_response(&json!([]), 0)),
        })
    }))
    .await;
    let store = fast_store(&server.url, Duration::from_secs(5));
    assert_eq!(store.list(&tenant()).await.unwrap(), Vec::new());
    assert_eq!(server.requests.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn permanent_http_errors_are_not_retried_and_never_leak_the_token() {
    let server = fake_server(Arc::new(|_, _| {
        let echo = format!("bad token secret-token {}", "x".repeat(10_000));
        Some((401, echo))
    }))
    .await;
    let store = fast_store(&server.url, Duration::from_secs(5));
    let message = store.list(&tenant()).await.unwrap_err().to_string();
    assert_eq!(server.requests.load(Ordering::SeqCst), 1);
    assert!(message.contains("HTTP 401"), "{message}");
    assert!(!message.contains("secret-token"), "{message}");
    assert!(message.contains("<redacted>"), "{message}");
    assert!(message.contains("truncated"), "{message}");
    assert!(
        message.len() < MAXIMUM_ERROR_BODY_BYTES + 512,
        "{}",
        message.len()
    );
}

#[tokio::test]
async fn write_errors_never_quote_a_body_echoing_the_state() {
    let server = fake_server(Arc::new(|_, body| {
        // Echo the request, state included, as some servers do.
        Some((
            400,
            json!({"message": format!("bad request {body}"), "code": "BAD_REQUEST"}).to_string(),
        ))
    }))
    .await;
    let store = fast_store(&server.url, Duration::from_secs(5));
    let record = ManagedResource {
        address: ResourceAddress::new("random_password", "database"),
        provider: "random".into(),
        provider_source: "registry.terraform.io/hashicorp/random".into(),
        schema_version: 0,
        state: json!({"result": "hunter2"}),
        private: Vec::new(),
        dependencies: Vec::new(),
        tainted: false,
        identity: None,
        serial: 0,
        generation: uuid::Uuid::nil(),
    };
    let lock = StateLock::generate();
    let message = store
        .put(&tenant(), &lock, &record)
        .await
        .unwrap_err()
        .to_string();
    assert!(message.contains("HTTP 400"), "{message}");
    assert!(message.contains("BAD_REQUEST"), "{message}");
    assert!(!message.contains("hunter2"), "{message}");
}

#[tokio::test]
async fn connection_failures_are_retried_and_report_their_cause() {
    // Bind then drop a listener to find a port nothing listens on.
    let port = TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let store = fast_store(&format!("http://127.0.0.1:{port}"), Duration::from_secs(5));
    let message = store.list(&tenant()).await.unwrap_err().to_string();
    assert!(message.contains("gave up after 4 attempts"), "{message}");
    // The source chain is included, not just "error sending request".
    assert!(message.to_lowercase().contains("connect"), "{message}");
    assert!(message.matches(": ").count() >= 2, "{message}");
}

#[tokio::test]
async fn timeouts_are_retried_and_reported_as_timeouts() {
    // Accept connections and never answer them.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let accepted = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&accepted);
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            counter.fetch_add(1, Ordering::SeqCst);
            // Read until the client gives up, never answering.
            tokio::spawn(async move {
                let mut ignored = Vec::new();
                let _ = stream.read_to_end(&mut ignored).await;
            });
        }
    });
    let store = fast_store(&url, Duration::from_millis(100));
    let message = store.list(&tenant()).await.unwrap_err().to_string();
    assert!(message.contains("timed out"), "{message}");
    assert!(message.contains("gave up after 4 attempts"), "{message}");
    assert_eq!(accepted.load(Ordering::SeqCst), 4);
}

#[tokio::test]
async fn lock_insert_with_lost_response_is_recovered_from_the_lock_row() {
    let inserted = Arc::new(Mutex::new(None::<String>));
    let recorded = Arc::clone(&inserted);
    let inserts = Arc::new(AtomicUsize::new(0));
    let insert_counter = Arc::clone(&inserts);
    let server = fake_server(Arc::new(move |_, body| {
        let sql = first_statement(&body);
        if schema_rows(&sql, LATEST_SCHEMA_VERSION).is_some() {
            return Some((
                200,
                pipeline_response(&body, |sql| schema_rows(sql, LATEST_SCHEMA_VERSION)),
            ));
        }
        if sql.starts_with("INSERT INTO cuenv_infrastructure_locks") {
            insert_counter.fetch_add(1, Ordering::SeqCst);
            // Commit the lock, then lose the response.
            let identifier = body["requests"][0]["stmt"]["args"][3]["value"]
                .as_str()
                .unwrap()
                .to_string();
            *recorded.lock().unwrap() = Some(identifier);
            return None;
        }
        let rows = recorded
            .lock()
            .unwrap()
            .as_ref()
            .map_or(json!([]), |identifier| {
                json!([[
                    {"type": "text", "value": identifier},
                    {"type": "text", "value": "test"},
                    {"type": "text", "value": "2026-01-01T00:00:00Z"},
                ]])
            });
        Some((200, execute_response(&rows, 0)))
    }))
    .await;
    let store = fast_store(&server.url, Duration::from_secs(5));
    let lock = store.lock(&tenant(), "test").await.unwrap();
    assert_eq!(Some(lock.lock_identifier), inserted.lock().unwrap().clone());
    // One insert and one read after the schema check: no blind retry of
    // the insert.
    assert_eq!(inserts.load(Ordering::SeqCst), 1);
    assert_eq!(server.requests.load(Ordering::SeqCst), 4);
}

#[tokio::test]
async fn lock_held_by_another_run_is_reported() {
    let server = fake_server(Arc::new(|_, body| {
        let sql = first_statement(&body);
        Some(if schema_rows(&sql, LATEST_SCHEMA_VERSION).is_some() {
            (
                200,
                pipeline_response(&body, |sql| schema_rows(sql, LATEST_SCHEMA_VERSION)),
            )
        } else if sql.starts_with("INSERT") {
            (200, execute_response(&json!([]), 0))
        } else {
            let rows = json!([[
                {"type": "text", "value": "other-lock"},
                {"type": "text", "value": "someone else"},
                {"type": "text", "value": "2026-01-01T00:00:00Z"},
            ]]);
            (200, execute_response(&rows, 0))
        })
    }))
    .await;
    let store = fast_store(&server.url, Duration::from_secs(5));
    let error = store.lock(&tenant(), "test").await.unwrap_err();
    assert!(
        matches!(&error, InfrastructureError::Locked { lock_identifier, holder, .. }
            if lock_identifier == "other-lock" && holder == "someone else"),
        "{error:?}"
    );
}

/// A fake database that answers with `rows_for` and records every
/// statement it receives.
async fn recording_database(
    rows_for: impl Fn(&str) -> Option<Value> + Send + Sync + 'static,
) -> (FakeServer, Arc<Mutex<Vec<String>>>) {
    let statements = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&statements);
    let server = fake_server(Arc::new(move |_, body| {
        let requests = body["requests"].as_array().unwrap();
        recorded.lock().unwrap().extend(
            requests
                .iter()
                .filter_map(|request| request["stmt"]["sql"].as_str())
                .map(str::to_string),
        );
        Some((200, pipeline_response(&body, &rows_for)))
    }))
    .await;
    (server, statements)
}

#[tokio::test]
async fn reads_on_a_database_without_cuenv_tables_are_empty_and_change_nothing() {
    let (server, statements) = recording_database(|sql| {
        sql.starts_with("SELECT name FROM sqlite_master")
            .then(|| json!([]))
    })
    .await;
    let store = fast_store(&server.url, Duration::from_secs(5));
    assert_eq!(store.list(&tenant()).await.unwrap(), Vec::new());
    assert_eq!(store.current_lock(&tenant()).await.unwrap(), None);
    assert!(!store.force_unlock(&tenant(), "any").await.unwrap());
    assert_eq!(store.owner(&tenant()).await.unwrap(), None);
    let statements = statements.lock().unwrap().clone();
    assert_eq!(statements.len(), 4, "{statements:?}");
    assert!(
        statements
            .iter()
            .all(|sql| sql.starts_with("SELECT name FROM sqlite_master")),
        "{statements:?}"
    );
    // Taking the lock needs a migrated schema.
    let message = store.lock(&tenant(), "test").await.unwrap_err().to_string();
    assert!(message.contains("migrate the state store"), "{message}");
}

#[tokio::test]
async fn every_operation_refuses_a_newer_schema() {
    let newer = LATEST_SCHEMA_VERSION + 1;
    let (server, statements) = recording_database(move |sql| {
        schema_rows(sql, newer).or_else(|| (sql == CREATE_SCHEMA_TABLE).then(|| json!([])))
    })
    .await;
    let store = fast_store(&server.url, Duration::from_secs(5));
    let messages = [
        store.migrate().await.unwrap_err().to_string(),
        store.list(&tenant()).await.unwrap_err().to_string(),
        store.current_lock(&tenant()).await.unwrap_err().to_string(),
        store.lock(&tenant(), "test").await.unwrap_err().to_string(),
        store
            .force_unlock(&tenant(), "lock")
            .await
            .unwrap_err()
            .to_string(),
        store.owner(&tenant()).await.unwrap_err().to_string(),
    ];
    for message in messages {
        assert!(
            message.contains(&format!(
                "the state schema is at version {newer}, newer than this cuenv supports"
            )),
            "{message}"
        );
    }
    assert!(matches!(
        store.list(&tenant()).await.unwrap_err(),
        InfrastructureError::StateSchemaNewer { found, supported }
            if found == newer && supported == LATEST_SCHEMA_VERSION
    ));
    // Nothing but schema inspection reached the database.
    let statements = statements.lock().unwrap().clone();
    assert!(
        statements.iter().all(|sql| sql == CREATE_SCHEMA_TABLE
            || sql == SELECT_SCHEMA_VERSION
            || sql.starts_with("SELECT name FROM sqlite_master")),
        "{statements:?}"
    );
}

/// One stored record as the server returns it for [`SELECT_RESOURCES`].
fn stored_row(generation: &Value) -> Value {
    json!([[
        {"type": "text", "value": "random_pet"},
        {"type": "text", "value": "pet"},
        {"type": "text", "value": "random"},
        {"type": "text", "value": "registry.terraform.io/hashicorp/random"},
        {"type": "integer", "value": "0"},
        {"type": "text", "value": "{\"id\":\"x\"}"},
        {"type": "null"},
        {"type": "text", "value": "[]"},
        {"type": "integer", "value": "0"},
        {"type": "null"},
        {"type": "integer", "value": "4"},
        generation,
    ]])
}

#[tokio::test]
async fn reads_decode_records_without_migrating_anything() {
    let generation = uuid::Uuid::from_u128(9).to_string();
    let (server, statements) = recording_database(move |sql| {
        schema_rows(sql, LATEST_SCHEMA_VERSION).or_else(|| {
            (sql == SELECT_RESOURCES)
                .then(|| stored_row(&json!({"type": "text", "value": generation})))
        })
    })
    .await;
    let store = fast_store(&server.url, Duration::from_secs(5));
    let resources = store.list(&tenant()).await.unwrap();
    assert_eq!(resources.len(), 1);
    assert_eq!(resources[0].serial, 4);
    assert_eq!(resources[0].generation, uuid::Uuid::from_u128(9));
    assert!(
        statements
            .lock()
            .unwrap()
            .iter()
            .all(|sql| !sql.starts_with("ALTER") && !sql.starts_with("CREATE")),
    );
}

#[tokio::test]
async fn an_undecodable_record_is_its_own_error_naming_the_address() {
    for generation in [
        json!({"type": "text", "value": ""}),
        json!({"type": "null"}),
        json!({"type": "text", "value": "not-a-uuid"}),
    ] {
        let (server, _) = recording_database(move |sql| {
            schema_rows(sql, LATEST_SCHEMA_VERSION)
                .or_else(|| (sql == SELECT_RESOURCES).then(|| stored_row(&generation)))
        })
        .await;
        let store = fast_store(&server.url, Duration::from_secs(5));
        let error = store.list(&tenant()).await.unwrap_err();
        assert!(
            matches!(&error, InfrastructureError::UndecodableRecord { address, problem }
                if address == "random_pet.pet" && problem.contains("generation")),
            "{error:?}"
        );
        let message = error.to_string();
        assert!(message.contains("random_pet.pet"), "{message}");
        assert!(!message.contains("Turso"), "{message}");
    }
}

#[test]
fn the_no_flag_identity_and_named_environments_are_distinct_key_values() {
    let none = TenantKey::new("example.com/app", "web").unwrap();
    let dev = TenantKey::with_environment("example.com/app", "web", "Dev").unwrap();
    let staging = TenantKey::with_environment("example.com/app", "web", "Staging").unwrap();
    assert_eq!(
        tenant_arguments(&none),
        vec![
            HranaValue::text("example.com/app"),
            HranaValue::text("web"),
            HranaValue::text(NO_ENVIRONMENT),
        ]
    );
    assert_eq!(tenant_arguments(&dev)[2], HranaValue::text("Dev"));
    assert_eq!(tenant_arguments(&staging)[2], HranaValue::text("Staging"));
    assert_ne!(tenant_arguments(&dev), tenant_arguments(&staging));
    assert_ne!(tenant_arguments(&none), tenant_arguments(&dev));
    // The empty key value belongs to the no-flag identity alone.
    assert!(TenantKey::with_environment("example.com/app", "web", "").is_err());
}

#[tokio::test]
async fn every_state_statement_is_scoped_by_environment() {
    let (server, statements) = recording_database(|sql| {
        schema_rows(sql, LATEST_SCHEMA_VERSION).or_else(|| {
            if sql.starts_with("SELECT instance, claimed_at FROM cuenv_infrastructure_owners") {
                Some(
                    json!([[{"type": "text", "value": ".:web"}, {"type": "text", "value": "now"}]]),
                )
            } else if sql.starts_with("SELECT ") {
                Some(json!([]))
            } else {
                (sql.starts_with("INSERT INTO cuenv_infrastructure_")
                    || sql.starts_with("DELETE FROM cuenv_infrastructure_")
                    || sql.starts_with("UPDATE cuenv_infrastructure_"))
                .then(|| json!([]))
            }
        })
    })
    .await;
    let store = fast_store(&server.url, Duration::from_secs(5));
    let named = TenantKey::with_environment("example.com/fake", "web", "Dev").unwrap();
    for tenant in [tenant(), named] {
        let lock = store.lock(&tenant, "test").await.unwrap();
        let record = ManagedResource {
            address: ResourceAddress::new("random_pet", "pet"),
            provider: "random".into(),
            provider_source: "registry.terraform.io/hashicorp/random".into(),
            schema_version: 0,
            state: json!({"id": "pet"}),
            private: Vec::new(),
            dependencies: Vec::new(),
            tainted: false,
            identity: None,
            serial: 0,
            generation: uuid::Uuid::nil(),
        };
        store.put(&tenant, &lock, &record).await.unwrap();
        store
            .put_if_unchanged(
                &tenant,
                &lock,
                &ConditionalPut {
                    resource: &record,
                    expected: RecordVersion::Generation {
                        generation: uuid::Uuid::nil(),
                        serial: 1,
                    },
                },
            )
            .await
            .unwrap();
        store.delete(&tenant, &lock, &record.address).await.unwrap();
        assert!(store.list(&tenant).await.unwrap().is_empty());
        assert!(
            store
                .read_resource(&tenant, &record.address)
                .await
                .unwrap()
                .is_none()
        );
        let instance = ProjectInstance::new(".", "web").unwrap();
        store
            .claim_owner(
                &tenant,
                &lock,
                &OwnerClaim {
                    instance: &instance,
                    mode: OwnerClaimMode::IfUnowned,
                },
            )
            .await
            .unwrap();
        assert!(store.current_lock(&tenant).await.unwrap().is_none());
        store
            .force_unlock(&tenant, &lock.lock_identifier)
            .await
            .unwrap();
        store.unlock(&tenant, &lock).await.unwrap();
    }
    let statements = statements.lock().unwrap();
    let state_statements: Vec<_> = statements
        .iter()
        .filter(|sql| sql.contains("cuenv_infrastructure_") && !sql.contains(SCHEMA_TABLE))
        .collect();
    assert!(state_statements.len() > 10, "{state_statements:?}");
    for sql in state_statements {
        assert!(
            sql.contains("environment = ?") || sql.contains("environment,"),
            "unscoped statement: {sql}"
        );
    }
}

#[tokio::test]
async fn redirects_are_not_followed() {
    let elsewhere = fake_server(Arc::new(|_, _| {
        Some((200, execute_response(&json!([]), 0)))
    }))
    .await;
    let redirecting = fake_server_with_headers(
        Arc::new(|_, _| Some((307, String::new()))),
        format!("location: {}/v2/pipeline\r\n", elsewhere.url),
    )
    .await;
    let store = fast_store(&redirecting.url, Duration::from_secs(5));
    let message = store.list(&tenant()).await.unwrap_err().to_string();
    assert!(message.contains("HTTP 307"), "{message}");
    assert_eq!(redirecting.requests.load(Ordering::SeqCst), 1);
    assert_eq!(elsewhere.requests.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn oversized_responses_are_refused_without_retrying() {
    let server = fake_server(Arc::new(|_, body| {
        Some((
            200,
            pipeline_response(&body, |_| {
                Some(json!([[{"type": "text", "value": "x".repeat(1_000)}]]))
            }),
        ))
    }))
    .await;
    let mut store = fast_store(&server.url, Duration::from_secs(5));
    store.maximum_response_bytes = 512;
    let message = store.list(&tenant()).await.unwrap_err().to_string();
    assert!(message.contains("exceeds the 512-byte limit"), "{message}");
    assert_eq!(server.requests.load(Ordering::SeqCst), 1);
}

/// Set in the child process of
/// [`plaintext_loopback_requests_bypass_proxy_environment`].
const PROXY_CHILD_VARIABLE: &str = "CUENV_INFRASTRUCTURE_TEST_PROXY_CHILD";

/// A plaintext URL is always loopback; its requests (and the token) must
/// never go to a proxy named in the environment. The proxy variables are
/// process-wide, so the check runs in a child process of this test binary.
#[tokio::test]
async fn plaintext_loopback_requests_bypass_proxy_environment() {
    if std::env::var_os(PROXY_CHILD_VARIABLE).is_some() {
        let server = fake_server(Arc::new(|_, body| {
            Some((200, pipeline_response(&body, |_| Some(json!([])))))
        }))
        .await;
        let store = fast_store(&server.url, Duration::from_secs(5));
        assert_eq!(store.list(&tenant()).await.unwrap(), Vec::new());
        assert_eq!(server.requests.load(Ordering::SeqCst), 1);
        return;
    }
    let proxy = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_url = format!("http://{}", proxy.local_addr().unwrap());
    let proxied = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&proxied);
    tokio::spawn(async move {
        // Accept and drop: a request sent here fails.
        while let Ok((stream, _)) = proxy.accept().await {
            counter.fetch_add(1, Ordering::SeqCst);
            drop(stream);
        }
    });
    let output = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "state::turso::tests::plaintext_loopback_requests_bypass_proxy_environment",
            "--test-threads=1",
        ])
        .env(PROXY_CHILD_VARIABLE, "1")
        .env("HTTP_PROXY", &proxy_url)
        .env("http_proxy", &proxy_url)
        .env("ALL_PROXY", &proxy_url)
        .env("all_proxy", &proxy_url)
        .env_remove("NO_PROXY")
        .env_remove("no_proxy")
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("1 passed"),
        "the child did not run the test"
    );
    assert_eq!(proxied.load(Ordering::SeqCst), 0);
}

#[test]
fn loopback_spellings_of_one_server_share_a_recovery_identity() {
    let identity = |url: &str| {
        TursoStateStore::new(TursoConfiguration {
            url: url.to_string(),
            authentication_token: None,
        })
        .unwrap()
        .recovery_identity()
        .unwrap()
    };
    let local = identity("http://localhost:8080");
    for spelling in [
        "http://127.0.0.1:8080",
        "http://[::1]:8080/",
        "ws://LOCALHOST:8080",
    ] {
        assert_eq!(identity(spelling), local, "{spelling}");
    }
    // A different port, another loopback address or a remote host is
    // another backend.
    for other in [
        "http://localhost:8081",
        "http://127.0.0.2:8080",
        "https://localhost:8080",
        "https://state.example.com",
    ] {
        assert_ne!(identity(other), local, "{other}");
    }
    assert_ne!(
        identity("https://state.example.com"),
        identity("https://state.example.org")
    );
}

#[tokio::test]
async fn a_lock_is_not_inserted_when_the_schema_moved_since_it_was_checked() {
    let version_reads = Arc::new(AtomicUsize::new(0));
    let reads = Arc::clone(&version_reads);
    let inserts = Arc::new(Mutex::new(Vec::<String>::new()));
    let recorded = Arc::clone(&inserts);
    let server = fake_server(Arc::new(move |_, body| {
        let sql = first_statement(&body);
        if sql == SELECT_SCHEMA_VERSION {
            // The check sees the current schema; a migration then commits,
            // so the next look sees a newer one.
            let version = if reads.fetch_add(1, Ordering::SeqCst) == 0 {
                LATEST_SCHEMA_VERSION
            } else {
                LATEST_SCHEMA_VERSION + 1
            };
            return Some((
                200,
                pipeline_response(&body, move |sql| schema_rows(sql, version)),
            ));
        }
        if schema_rows(&sql, LATEST_SCHEMA_VERSION).is_some() {
            return Some((
                200,
                pipeline_response(&body, |sql| schema_rows(sql, LATEST_SCHEMA_VERSION)),
            ));
        }
        if sql.starts_with("INSERT INTO cuenv_infrastructure_locks") {
            recorded.lock().unwrap().push(sql);
        }
        // The guarded insert writes nothing, and no lock row exists.
        Some((200, execute_response(&json!([]), 0)))
    }))
    .await;
    let store = fast_store(&server.url, Duration::from_secs(5));
    let error = store.lock(&tenant(), "test").await.unwrap_err();
    assert!(
        matches!(&error, InfrastructureError::StateSchemaNewer { found, supported }
            if *found == LATEST_SCHEMA_VERSION + 1 && *supported == LATEST_SCHEMA_VERSION),
        "{error:?}"
    );
    let inserts = inserts.lock().unwrap();
    assert_eq!(inserts.len(), 1, "the insert is not retried: {inserts:?}");
    assert!(
        inserts[0].contains(&format!(
            "WHERE ({SELECT_SCHEMA_VERSION}) = {LATEST_SCHEMA_VERSION}"
        )),
        "{}",
        inserts[0]
    );
}

/// Waits short enough for tests, in the proportions of the real ones.
fn short_migration_wait() -> MigrationWait {
    MigrationWait {
        bound: Duration::from_millis(600),
        poll_interval: Duration::from_millis(50),
        announcement_lifetime: Duration::from_secs(30),
    }
}

fn table_names(names: &[&str]) -> Value {
    Value::Array(
        names
            .iter()
            .map(|name| json!([{"type": "text", "value": name}]))
            .collect(),
    )
}

#[test]
fn tables_of_unreleased_builds_are_told_apart_from_foreign_ones() {
    let names = |names: &[&str]| -> Vec<String> { names.iter().map(ToString::to_string).collect() };
    assert_eq!(classify_layout(&[]), Layout::Recognized);
    // Names that merely start alike are not cuenv's.
    assert_eq!(
        classify_layout(&names(&["cuenv_infrastructure_notes"])),
        Layout::Recognized
    );
    for unreleased in [
        "cuenv_infrastructure_schema",
        "cuenv_infrastructure_environment_resources",
        "cuenv_infrastructure_environment_locks",
        "cuenv_infrastructurestructure_resources",
    ] {
        assert_eq!(
            classify_layout(&names(&["cuenv_infrastructure_resources", unreleased])),
            Layout::Unreleased(names(&[unreleased])),
            "{unreleased}"
        );
    }
    assert_eq!(
        classify_layout(&names(&["cuenv_infrastructure_locks"])),
        Layout::Unrecorded(names(&["cuenv_infrastructure_locks"]))
    );
}

#[tokio::test]
async fn a_development_layout_is_named_never_called_newer_and_never_ignored() {
    for existing in [
        // The schema table of a development build, which recorded versions
        // 1 to 5 in it.
        vec![DEVELOPMENT_SCHEMA_TABLE, RESOURCES_TABLE],
        // The earliest layout, whose table names were misspelled.
        vec!["cuenv_infrastructurestructure_resources"],
    ] {
        let tables = existing.clone();
        let (server, _) = recording_database(move |sql| {
            if sql.contains("LIKE") {
                Some(table_names(&tables))
            } else if sql.starts_with("SELECT name FROM sqlite_master") {
                // No schema table of this cuenv.
                Some(json!([]))
            } else {
                None
            }
        })
        .await;
        let store = fast_store(&server.url, Duration::from_secs(5));
        let errors = [
            store.list(&tenant()).await.unwrap_err(),
            store.current_lock(&tenant()).await.unwrap_err(),
            store.locks().await.unwrap_err(),
            store.migrate().await.unwrap_err(),
            store.lock(&tenant(), "test").await.unwrap_err(),
        ];
        for error in errors {
            let message = error.to_string();
            assert!(
                matches!(&error, InfrastructureError::StateUnreleasedLayout { tables }
                    if tables.iter().all(|table| existing.contains(&table.as_str()))),
                "{message}"
            );
            assert!(
                message.contains("unreleased development build of cuenv"),
                "{message}"
            );
            assert!(!message.contains("newer"), "{message}");
        }
    }
}

#[tokio::test]
async fn tables_with_cuenvs_names_but_no_migration_record_are_a_schema_conflict() {
    let (server, statements) = recording_database(|sql| {
        if sql.contains("LIKE") {
            Some(table_names(&[RESOURCES_TABLE, LOCKS_TABLE]))
        } else if sql.starts_with("SELECT name FROM sqlite_master") {
            Some(json!([]))
        } else {
            None
        }
    })
    .await;
    let store = fast_store(&server.url, Duration::from_secs(5));
    for error in [
        store.list(&tenant()).await.unwrap_err(),
        store.migrate().await.unwrap_err(),
    ] {
        let message = error.to_string();
        assert!(
            matches!(error, InfrastructureError::StateSchemaConflict { .. }),
            "{message}"
        );
        assert!(message.contains(RESOURCES_TABLE), "{message}");
        assert!(message.contains("did not create them"), "{message}");
    }
    // Migrating never tried to create over them.
    let statements = statements.lock().unwrap();
    assert!(
        statements.iter().all(|sql| !sql.starts_with("CREATE")),
        "{statements:?}"
    );
}

#[test]
fn only_the_overflow_of_the_fence_means_a_lock_is_held() {
    let transaction = MigrationTransaction::of(&Migration {
        version: 2,
        statements: &["CREATE TABLE cuenv_infrastructure_probe (value INTEGER NOT NULL)"],
    });
    let fence = transaction.fence_index.unwrap();
    let failed = |message: &str| {
        let mut errors: Vec<Option<HranaError>> =
            (0..transaction.statements.len()).map(|_| None).collect();
        errors[fence] = Some(HranaError {
            message: message.to_string(),
            code: None,
        });
        BatchResult {
            step_results: Vec::new(),
            step_errors: errors,
        }
    };
    assert_eq!(
        migration_outcome(&failed("SQLite error: integer overflow"), &transaction).unwrap(),
        MigrationOutcome::LockHeld
    );
    // Any other failure of the fence statement is not a held lock.
    for message in [
        "SQLite error: no such table: cuenv_infrastructure_locks",
        "database is locked",
    ] {
        let failure = migration_outcome(&failed(message), &transaction).unwrap_err();
        assert!(failure.message.contains(message), "{}", failure.message);
    }
}

#[tokio::test]
async fn locks_are_listed_from_the_database_for_every_tenant() {
    let (server, _) = recording_database(|sql| {
        if sql == SELECT_ALL_LOCKS {
            Some(json!([
                [
                    {"type": "text", "value": "example.com/a"},
                    {"type": "text", "value": "api"},
                    {"type": "text", "value": ""},
                    {"type": "text", "value": "id-1"},
                    {"type": "text", "value": "apply by\u{1b}[31m ci"},
                    {"type": "text", "value": "2026-01-01T00:00:00+00:00"},
                ],
                [
                    {"type": "text", "value": "example.com/a"},
                    {"type": "text", "value": "api"},
                    {"type": "text", "value": "Dev"},
                    {"type": "text", "value": "id-2"},
                    {"type": "text", "value": "destroy"},
                    {"type": "text", "value": "2026-01-02T00:00:00+00:00"},
                ],
            ]))
        } else {
            schema_rows(sql, LATEST_SCHEMA_VERSION)
        }
    })
    .await;
    let store = fast_store(&server.url, Duration::from_secs(5));
    let locks = store.locks().await.unwrap();
    assert_eq!(locks.len(), 2);
    assert_eq!(locks[0].label(), "example.com/a#api");
    assert_eq!(locks[0].lock.holder, "apply by[31m ci");
    assert_eq!(locks[1].label(), "example.com/a#api@Dev");
    assert_eq!(locks[1].tenant().unwrap().environment(), Some("Dev"));
    assert!(locks[0].lock.age_description().is_some());
}

#[tokio::test]
async fn addresses_are_read_from_the_key_columns_alone() {
    let (server, statements) = recording_database(|sql| {
        if sql == SELECT_ADDRESSES {
            Some(json!([[
                {"type": "text", "value": "random_pet"},
                {"type": "text", "value": "pet"},
            ]]))
        } else {
            schema_rows(sql, LATEST_SCHEMA_VERSION)
        }
    })
    .await;
    let store = fast_store(&server.url, Duration::from_secs(5));
    assert_eq!(
        store.addresses(&tenant()).await.unwrap(),
        vec![ResourceAddress::new("random_pet", "pet")]
    );
    assert!(
        statements
            .lock()
            .unwrap()
            .iter()
            .all(|sql| !sql.contains("state_json")),
    );
}

/// Against a real server: tables of a development build and foreign tables
/// are refused by name, and a waiting migration holds off new locks. Needs an
/// empty isolated database; it drops cuenv's tables at the end.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires an empty isolated libSQL database (CUENV_INFRASTRUCTURE_TEST_TURSO_LAYOUT_URL)"]
async fn layouts_and_waiting_migrations_against_a_server() {
    let url = std::env::var("CUENV_INFRASTRUCTURE_TEST_TURSO_LAYOUT_URL").unwrap();
    let mut store = TursoStateStore::new(TursoConfiguration {
        url,
        authentication_token: std::env::var("TURSO_AUTH_TOKEN").ok(),
    })
    .unwrap();
    store.migration_wait = short_migration_wait();
    let store = Arc::new(store);
    let scenario = tokio::spawn({
        let store = Arc::clone(&store);
        async move { layout_scenario(&store).await }
    });
    let outcome = scenario.await;
    drop_state_tables(&store).await;
    outcome.unwrap().unwrap();
}

async fn layout_scenario(store: &Arc<TursoStateStore>) -> Result<()> {
    let create = |sql: &str| store.execute(Statement::new(sql, Vec::new()));
    // A development build's schema table: refused as such, by every
    // operation, and nothing of this cuenv is created.
    create("CREATE TABLE cuenv_infrastructure_schema (version INTEGER NOT NULL)").await?;
    create("INSERT INTO cuenv_infrastructure_schema (version) VALUES (5)").await?;
    for error in [
        store.list(&tenant()).await.unwrap_err(),
        store.migrate().await.unwrap_err(),
        store.locks().await.unwrap_err(),
    ] {
        assert!(
            matches!(error, InfrastructureError::StateUnreleasedLayout { .. }),
            "{error:?}"
        );
    }
    assert!(!table_exists(store, SCHEMA_TABLE).await);
    create("DROP TABLE cuenv_infrastructure_schema").await?;

    // Tables with cuenv's names and no migration record.
    create("CREATE TABLE cuenv_infrastructure_resources (value INTEGER)").await?;
    let error = store.migrate().await.unwrap_err();
    assert!(
        matches!(error, InfrastructureError::StateSchemaConflict { .. }),
        "{error:?}"
    );
    create("DROP TABLE cuenv_infrastructure_resources").await?;

    // A clean database migrates; then a migration that waits for a lock
    // holds new locks off, and gives up with the lock listed.
    store.migrate().await?;
    let tenant = tenant();
    let held = store.lock(&tenant, "stale").await?;
    let migrating = {
        let store = Arc::clone(store);
        tokio::spawn(async move { store.migrate_to(&migrations_with_a_probe()).await })
    };
    tokio::time::sleep(Duration::from_millis(250)).await;
    let other = TenantKey::new("example.com/fake", "api")?;
    let refused = store.lock(&other, "newcomer").await.unwrap_err();
    assert!(
        matches!(
            refused,
            InfrastructureError::StateMigrationPending { version: 2 }
        ),
        "{refused:?}"
    );
    let blocked = migrating.await.unwrap().unwrap_err();
    assert!(
        matches!(&blocked, InfrastructureError::StateMigrationBlocked { locks, .. } if locks.len() == 1),
        "{blocked:?}"
    );
    // The announcement is withdrawn once the migration gave up.
    let lock = store.lock(&other, "newcomer").await?;
    store.unlock(&other, &lock).await?;
    // A lock released while it waits lets it through.
    let migrating = {
        let store = Arc::clone(store);
        tokio::spawn(async move { store.migrate_to(&migrations_with_a_probe()).await })
    };
    tokio::time::sleep(Duration::from_millis(150)).await;
    store.unlock(&tenant, &held).await?;
    migrating.await.unwrap()?;
    assert_eq!(store.schema_version().await?, 2);
    Ok(())
}
