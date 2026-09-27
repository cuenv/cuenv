//! Turso (libSQL) state store over the Hrana HTTP protocol.
//!
//! Talks to `POST {url}/v2/pipeline` with bearer-token auth, so it works
//! against Turso Cloud databases and self-hosted `sqld` alike without a
//! native libSQL dependency.

use std::fmt;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};
use serde::{Deserialize, Serialize};

use super::{ManagedResource, ResourceAddress, StateLock, StateStore};
use crate::error::{InfraError, Result};
use crate::tenant::TenantKey;

const MIGRATIONS: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS cuenv_infra_resources (
        module_path TEXT NOT NULL,
        project TEXT NOT NULL,
        resource_type TEXT NOT NULL,
        resource_name TEXT NOT NULL,
        provider TEXT NOT NULL,
        provider_source TEXT NOT NULL,
        schema_version INTEGER NOT NULL,
        state_json TEXT NOT NULL,
        private BLOB,
        dependencies_json TEXT NOT NULL DEFAULT '[]',
        serial INTEGER NOT NULL DEFAULT 1,
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL,
        PRIMARY KEY (module_path, project, resource_type, resource_name)
    ) WITHOUT ROWID",
    "CREATE TABLE IF NOT EXISTS cuenv_infra_locks (
        module_path TEXT NOT NULL,
        project TEXT NOT NULL,
        lock_id TEXT NOT NULL,
        holder TEXT NOT NULL,
        acquired_at TEXT NOT NULL,
        PRIMARY KEY (module_path, project)
    ) WITHOUT ROWID",
];

/// Connection settings for a Turso database.
#[derive(Clone)]
pub struct TursoConfig {
    /// Database URL: `libsql://`, `https://` or `http://` (local `sqld`).
    pub url: String,
    /// Database auth token. Optional for unauthenticated local `sqld`.
    pub auth_token: Option<String>,
}

impl fmt::Debug for TursoConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TursoConfig")
            .field("url", &self.url)
            .field(
                "auth_token",
                &self.auth_token.as_ref().map(|_| "<redacted>"),
            )
            .finish()
    }
}

/// [`StateStore`] backed by a remote Turso database.
#[derive(Debug, Clone)]
pub struct TursoStateStore {
    client: reqwest::Client,
    pipeline_url: String,
    auth_token: Option<String>,
}

impl TursoStateStore {
    /// Create a store for the given database.
    ///
    /// # Errors
    ///
    /// Returns [`InfraError::Config`] for unsupported URL schemes and
    /// [`InfraError::State`] if the HTTP client cannot be built.
    pub fn new(config: TursoConfig) -> Result<Self> {
        crate::ensure_rustls_crypto_provider();
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| InfraError::state(format!("failed to build HTTP client: {e}")))?;
        Ok(Self {
            client,
            pipeline_url: pipeline_url(&config.url)?,
            auth_token: config.auth_token.filter(|t| !t.is_empty()),
        })
    }

    async fn pipeline(&self, statements: Vec<Stmt>) -> Result<Vec<ExecuteResult>> {
        let count = statements.len();
        let mut requests: Vec<PipelineRequest> = statements
            .into_iter()
            .map(|stmt| PipelineRequest::Execute { stmt })
            .collect();
        requests.push(PipelineRequest::Close);
        let body = PipelineBody {
            baton: None,
            requests,
        };

        let mut request = self.client.post(&self.pipeline_url).json(&body);
        if let Some(token) = &self.auth_token {
            request = request.bearer_auth(token);
        }
        let response = request
            .send()
            .await
            .map_err(|e| InfraError::state(format!("Turso request failed: {e}")))?;
        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(InfraError::state(format!(
                "Turso returned HTTP {status}: {text}"
            )));
        }
        let parsed: PipelineResponse = response
            .json()
            .await
            .map_err(|e| InfraError::state(format!("invalid Turso response: {e}")))?;

        let mut results = Vec::with_capacity(count);
        for entry in parsed.results.into_iter().take(count) {
            match entry {
                PipelineResult::Ok {
                    response: StreamResponse::Execute { result },
                } => results.push(result),
                PipelineResult::Ok { .. } => {
                    return Err(InfraError::state("unexpected Turso response type"));
                }
                PipelineResult::Error { error } => {
                    return Err(InfraError::state(format!(
                        "Turso statement failed: {}{}",
                        error.message,
                        error.code.map(|c| format!(" ({c})")).unwrap_or_default()
                    )));
                }
            }
        }
        if results.len() != count {
            return Err(InfraError::state(format!(
                "Turso returned {} results for {count} statements",
                results.len()
            )));
        }
        Ok(results)
    }

    async fn execute(&self, stmt: Stmt) -> Result<ExecuteResult> {
        self.pipeline(vec![stmt])
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| InfraError::state("Turso returned no result"))
    }
}

/// Convert a database URL into the Hrana v2 pipeline endpoint.
fn pipeline_url(url: &str) -> Result<String> {
    let url = url.trim().trim_end_matches('/');
    let base = if let Some(rest) = url.strip_prefix("libsql://") {
        format!("https://{rest}")
    } else if let Some(rest) = url.strip_prefix("wss://") {
        format!("https://{rest}")
    } else if let Some(rest) = url.strip_prefix("ws://") {
        format!("http://{rest}")
    } else if url.starts_with("https://") || url.starts_with("http://") {
        url.to_string()
    } else {
        return Err(InfraError::config(format!(
            "unsupported Turso URL '{url}'; expected libsql://, https:// or http://"
        )));
    };
    Ok(format!("{base}/v2/pipeline"))
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

#[async_trait]
impl StateStore for TursoStateStore {
    async fn migrate(&self) -> Result<()> {
        self.pipeline(
            MIGRATIONS
                .iter()
                .map(|sql| Stmt::new(*sql, Vec::new()))
                .collect(),
        )
        .await
        .map(|_| ())
    }

    async fn list(&self, tenant: &TenantKey) -> Result<Vec<ManagedResource>> {
        let result = self
            .execute(Stmt::new(
                "SELECT resource_type, resource_name, provider, provider_source, \
                 schema_version, state_json, private, dependencies_json \
                 FROM cuenv_infra_resources WHERE module_path = ? AND project = ? \
                 ORDER BY resource_type, resource_name",
                vec![
                    HranaValue::text(tenant.module_path()),
                    HranaValue::text(tenant.project()),
                ],
            ))
            .await?;
        result.rows.iter().map(|row| row_to_resource(row)).collect()
    }

    async fn put(&self, tenant: &TenantKey, resource: &ManagedResource) -> Result<()> {
        let state_json = serde_json::to_string(&resource.state)
            .map_err(|e| InfraError::state(format!("serialize state: {e}")))?;
        let dependencies = serde_json::to_string(&resource.dependencies)
            .map_err(|e| InfraError::state(format!("serialize dependencies: {e}")))?;
        let ts = now();
        self.execute(Stmt::new(
            "INSERT INTO cuenv_infra_resources (module_path, project, resource_type, \
             resource_name, provider, provider_source, schema_version, state_json, private, \
             dependencies_json, serial, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1, ?, ?) \
             ON CONFLICT (module_path, project, resource_type, resource_name) DO UPDATE SET \
             provider = excluded.provider, provider_source = excluded.provider_source, \
             schema_version = excluded.schema_version, state_json = excluded.state_json, \
             private = excluded.private, dependencies_json = excluded.dependencies_json, \
             serial = cuenv_infra_resources.serial + 1, updated_at = excluded.updated_at",
            vec![
                HranaValue::text(tenant.module_path()),
                HranaValue::text(tenant.project()),
                HranaValue::text(&resource.address.resource_type),
                HranaValue::text(&resource.address.name),
                HranaValue::text(&resource.provider),
                HranaValue::text(&resource.provider_source),
                HranaValue::integer(resource.schema_version),
                HranaValue::text(&state_json),
                HranaValue::blob(&resource.private),
                HranaValue::text(&dependencies),
                HranaValue::text(&ts),
                HranaValue::text(&ts),
            ],
        ))
        .await
        .map(|_| ())
    }

    async fn delete(&self, tenant: &TenantKey, address: &ResourceAddress) -> Result<()> {
        self.execute(Stmt::new(
            "DELETE FROM cuenv_infra_resources WHERE module_path = ? AND project = ? \
             AND resource_type = ? AND resource_name = ?",
            vec![
                HranaValue::text(tenant.module_path()),
                HranaValue::text(tenant.project()),
                HranaValue::text(&address.resource_type),
                HranaValue::text(&address.name),
            ],
        ))
        .await
        .map(|_| ())
    }

    async fn lock(&self, tenant: &TenantKey, holder: &str) -> Result<StateLock> {
        let lock_id = uuid::Uuid::new_v4().to_string();
        let inserted = self
            .execute(Stmt::new(
                "INSERT INTO cuenv_infra_locks (module_path, project, lock_id, holder, acquired_at) \
                 VALUES (?, ?, ?, ?, ?) ON CONFLICT (module_path, project) DO NOTHING",
                vec![
                    HranaValue::text(tenant.module_path()),
                    HranaValue::text(tenant.project()),
                    HranaValue::text(&lock_id),
                    HranaValue::text(holder),
                    HranaValue::text(&now()),
                ],
            ))
            .await?;
        if inserted.affected_row_count == 1 {
            return Ok(StateLock { lock_id });
        }

        let existing = self
            .execute(Stmt::new(
                "SELECT lock_id, holder, acquired_at FROM cuenv_infra_locks \
                 WHERE module_path = ? AND project = ?",
                vec![
                    HranaValue::text(tenant.module_path()),
                    HranaValue::text(tenant.project()),
                ],
            ))
            .await?;
        let row = existing.rows.into_iter().next().unwrap_or_default();
        let field = |i: usize| {
            row.get(i)
                .and_then(HranaValue::as_text)
                .unwrap_or("unknown")
                .to_string()
        };
        Err(InfraError::Locked {
            tenant: tenant.to_string(),
            lock_id: field(0),
            holder: field(1),
            acquired_at: field(2),
        })
    }

    async fn unlock(&self, tenant: &TenantKey, lock: &StateLock) -> Result<()> {
        self.execute(Stmt::new(
            "DELETE FROM cuenv_infra_locks WHERE module_path = ? AND project = ? AND lock_id = ?",
            vec![
                HranaValue::text(tenant.module_path()),
                HranaValue::text(tenant.project()),
                HranaValue::text(&lock.lock_id),
            ],
        ))
        .await
        .map(|_| ())
    }

    async fn force_unlock(&self, tenant: &TenantKey) -> Result<()> {
        self.execute(Stmt::new(
            "DELETE FROM cuenv_infra_locks WHERE module_path = ? AND project = ?",
            vec![
                HranaValue::text(tenant.module_path()),
                HranaValue::text(tenant.project()),
            ],
        ))
        .await
        .map(|_| ())
    }
}

fn row_to_resource(row: &[HranaValue]) -> Result<ManagedResource> {
    let text = |i: usize, name: &str| {
        row.get(i)
            .and_then(HranaValue::as_text)
            .map(str::to_string)
            .ok_or_else(|| InfraError::state(format!("state row missing {name}")))
    };
    let state_json = text(5, "state_json")?;
    let dependencies_json = text(7, "dependencies_json")?;
    Ok(ManagedResource {
        address: ResourceAddress::new(text(0, "resource_type")?, text(1, "resource_name")?),
        provider: text(2, "provider")?,
        provider_source: text(3, "provider_source")?,
        schema_version: row
            .get(4)
            .and_then(HranaValue::as_integer)
            .ok_or_else(|| InfraError::state("state row missing schema_version"))?,
        state: serde_json::from_str(&state_json)
            .map_err(|e| InfraError::state(format!("corrupt state_json: {e}")))?,
        private: row
            .get(6)
            .map(HranaValue::as_blob)
            .transpose()?
            .flatten()
            .unwrap_or_default(),
        dependencies: serde_json::from_str(&dependencies_json)
            .map_err(|e| InfraError::state(format!("corrupt dependencies_json: {e}")))?,
    })
}

// ---------------------------------------------------------------------------
// Hrana over HTTP wire types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct PipelineBody {
    baton: Option<String>,
    requests: Vec<PipelineRequest>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum PipelineRequest {
    Execute { stmt: Stmt },
    Close,
}

#[derive(Debug, Serialize)]
struct Stmt {
    sql: String,
    args: Vec<HranaValue>,
    want_rows: bool,
}

impl Stmt {
    fn new(sql: impl Into<String>, args: Vec<HranaValue>) -> Self {
        Self {
            sql: sql.into(),
            args,
            want_rows: true,
        }
    }
}

#[derive(Debug, Deserialize)]
struct PipelineResponse {
    results: Vec<PipelineResult>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum PipelineResult {
    Ok { response: StreamResponse },
    Error { error: HranaError },
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum StreamResponse {
    Execute {
        result: ExecuteResult,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Default, Deserialize)]
struct ExecuteResult {
    #[serde(default)]
    rows: Vec<Vec<HranaValue>>,
    #[serde(default)]
    affected_row_count: u64,
}

#[derive(Debug, Deserialize)]
struct HranaError {
    message: String,
    #[serde(default)]
    code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum HranaValue {
    Null,
    Integer { value: String },
    Float { value: f64 },
    Text { value: String },
    Blob { base64: String },
}

impl HranaValue {
    fn text(value: &str) -> Self {
        Self::Text {
            value: value.to_string(),
        }
    }

    fn integer(value: i64) -> Self {
        Self::Integer {
            value: value.to_string(),
        }
    }

    fn blob(bytes: &[u8]) -> Self {
        if bytes.is_empty() {
            return Self::Null;
        }
        Self::Blob {
            base64: STANDARD_NO_PAD.encode(bytes),
        }
    }

    fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text { value } => Some(value),
            _ => None,
        }
    }

    fn as_integer(&self) -> Option<i64> {
        match self {
            Self::Integer { value } => value.parse().ok(),
            _ => None,
        }
    }

    fn as_blob(&self) -> Result<Option<Vec<u8>>> {
        match self {
            Self::Blob { base64 } => {
                let trimmed = base64.trim_end_matches('=');
                STANDARD_NO_PAD
                    .decode(trimmed)
                    .or_else(|_| STANDARD.decode(base64))
                    .map(Some)
                    .map_err(|e| InfraError::state(format!("corrupt private blob: {e}")))
            }
            _ => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn pipeline_url_normalizes_schemes() {
        assert_eq!(
            pipeline_url("libsql://db-acme.turso.io").unwrap(),
            "https://db-acme.turso.io/v2/pipeline"
        );
        assert_eq!(
            pipeline_url("http://127.0.0.1:8080/").unwrap(),
            "http://127.0.0.1:8080/v2/pipeline"
        );
        assert_eq!(
            pipeline_url("wss://db.turso.io").unwrap(),
            "https://db.turso.io/v2/pipeline"
        );
        assert!(pipeline_url("postgres://nope").is_err());
    }

    #[test]
    fn pipeline_body_matches_hrana_wire_format() {
        let body = PipelineBody {
            baton: None,
            requests: vec![
                PipelineRequest::Execute {
                    stmt: Stmt::new(
                        "SELECT ?",
                        vec![
                            HranaValue::text("a"),
                            HranaValue::integer(7),
                            HranaValue::blob(&[1, 2, 3]),
                            HranaValue::blob(&[]),
                        ],
                    ),
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
                    ]}},
                    {"type": "close"},
                ],
            })
        );
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
    fn config_debug_redacts_token() {
        let config = TursoConfig {
            url: "libsql://db.turso.io".into(),
            auth_token: Some("secret-token".into()),
        };
        let rendered = format!("{config:?}");
        assert!(!rendered.contains("secret-token"));
        assert!(rendered.contains("<redacted>"));
    }
}
