//! Google Cloud Secret Manager resolver.

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use cuenv_secrets::http::{build_client, env_var, normalize_api_url};
use cuenv_secrets::{SecretError, SecretResolver, SecretSpec};
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use tokio::process::Command;

const DEFAULT_SECRET_MANAGER_URL: &str = "https://secretmanager.googleapis.com";

/// Configuration for resolving a single Google Cloud Secret Manager secret.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GcpSecretConfig {
    /// Google Cloud project ID or number.
    pub project: String,

    /// Secret Manager secret ID.
    pub secret: String,

    /// Secret version to access.
    #[serde(default = "default_version")]
    pub version: String,

    /// Optional Secret Manager API base URL, primarily for tests or private endpoints.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_url: Option<String>,
}

impl GcpSecretConfig {
    /// Create a new Google Cloud Secret Manager config.
    #[must_use]
    pub fn new(project: impl Into<String>, secret: impl Into<String>) -> Self {
        Self {
            project: project.into(),
            secret: secret.into(),
            version: default_version(),
            api_url: None,
        }
    }

    fn validate(&self, name: &str) -> Result<(), SecretError> {
        if self.project.trim().is_empty() {
            return Err(SecretError::resolution_failed(
                name,
                "GCP project cannot be empty",
            ));
        }
        if self.secret.trim().is_empty() {
            return Err(SecretError::resolution_failed(
                name,
                "GCP secret cannot be empty",
            ));
        }
        if self.version.trim().is_empty() {
            return Err(SecretError::resolution_failed(
                name,
                "GCP secret version cannot be empty",
            ));
        }
        Ok(())
    }

    fn api_url(&self) -> String {
        normalize_api_url(
            self.api_url
                .as_deref()
                .unwrap_or(DEFAULT_SECRET_MANAGER_URL),
        )
    }
}

/// Resolves secrets from Google Cloud Secret Manager.
///
/// Authentication uses `GOOGLE_OAUTH_ACCESS_TOKEN` when present. Otherwise the
/// resolver asks `gcloud auth application-default print-access-token` for an
/// Application Default Credentials token. When `GOOGLE_APPLICATION_CREDENTIALS`
/// points to a service account JSON file, the path is also provided to gcloud as
/// `CLOUDSDK_AUTH_CREDENTIAL_FILE_OVERRIDE`.
pub struct GcpSecretManagerResolver {
    client: Client,
}

impl std::fmt::Debug for GcpSecretManagerResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GcpSecretManagerResolver")
            .finish_non_exhaustive()
    }
}

impl GcpSecretManagerResolver {
    /// Create a new Google Cloud Secret Manager resolver.
    ///
    /// # Errors
    ///
    /// Returns an error if the HTTP client cannot be initialized.
    pub fn new() -> Result<Self, SecretError> {
        Ok(Self {
            client: build_client("gcp")?,
        })
    }

    fn parse_config(name: &str, spec: &SecretSpec) -> Result<GcpSecretConfig, SecretError> {
        let config: GcpSecretConfig = serde_json::from_str(&spec.source).map_err(|e| {
            SecretError::resolution_failed(
                name,
                format!("GCP resolver requires structured config: {e}"),
            )
        })?;
        config.validate(name)?;
        Ok(config)
    }

    async fn access_token(name: &str) -> Result<String, SecretError> {
        if let Some(token) = env_var("GOOGLE_OAUTH_ACCESS_TOKEN") {
            return Ok(token);
        }

        let mut command = Command::new("gcloud");
        command.args(["auth", "application-default", "print-access-token"]);

        if let Some(credentials) = env_var("GOOGLE_APPLICATION_CREDENTIALS") {
            command.env("GOOGLE_APPLICATION_CREDENTIALS", &credentials);
            command.env("CLOUDSDK_AUTH_CREDENTIAL_FILE_OVERRIDE", credentials);
        }

        let output = command.output().await.map_err(|e| {
            SecretError::resolution_failed(
                name,
                format!(
                    "Failed to run gcloud for Application Default Credentials: {e}. \
                    Set GOOGLE_OAUTH_ACCESS_TOKEN, run `gcloud auth application-default login`, \
                    or set GOOGLE_APPLICATION_CREDENTIALS to a service account JSON file"
                ),
            )
        })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(SecretError::resolution_failed(
                name,
                format!(
                    "gcloud could not provide an Application Default Credentials token: {}",
                    stderr.trim()
                ),
            ));
        }

        let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if token.is_empty() {
            return Err(SecretError::resolution_failed(
                name,
                "gcloud returned an empty Application Default Credentials token",
            ));
        }
        Ok(token)
    }

    async fn send_secret_request(
        &self,
        name: &str,
        config: &GcpSecretConfig,
        token: &str,
    ) -> Result<reqwest::Response, SecretError> {
        let url = secret_url(name, config)?;
        self.client
            .get(url)
            .bearer_auth(token)
            .send()
            .await
            .map_err(|e| {
                SecretError::resolution_failed(name, format!("GCP Secret Manager read failed: {e}"))
            })
    }

    async fn parse_secret_response(
        name: &str,
        response: reqwest::Response,
    ) -> Result<String, SecretError> {
        let status = response.status();
        if !status.is_success() {
            return Err(SecretError::resolution_failed(
                name,
                format!("GCP Secret Manager read failed with HTTP {status}"),
            ));
        }

        let body = response
            .json::<AccessSecretVersionResponse>()
            .await
            .map_err(|e| {
                SecretError::resolution_failed(
                    name,
                    format!("Failed to parse GCP Secret Manager response: {e}"),
                )
            })?;

        let bytes = STANDARD.decode(body.payload.data).map_err(|e| {
            SecretError::resolution_failed(
                name,
                format!("Failed to decode GCP Secret Manager payload: {e}"),
            )
        })?;

        String::from_utf8(bytes).map_err(|e| {
            SecretError::resolution_failed(
                name,
                format!("GCP Secret Manager payload is not valid UTF-8: {e}"),
            )
        })
    }
}

#[async_trait]
impl SecretResolver for GcpSecretManagerResolver {
    async fn resolve(&self, name: &str, spec: &SecretSpec) -> Result<String, SecretError> {
        let config = Self::parse_config(name, spec)?;
        let token = Self::access_token(name).await?;
        let response = self.send_secret_request(name, &config, &token).await?;
        Self::parse_secret_response(name, response).await
    }

    fn provider_name(&self) -> &'static str {
        "gcp"
    }
}

#[derive(Deserialize)]
struct AccessSecretVersionResponse {
    payload: SecretPayload,
}

#[derive(Deserialize)]
struct SecretPayload {
    data: String,
}

fn secret_url(name: &str, config: &GcpSecretConfig) -> Result<Url, SecretError> {
    let mut url = Url::parse(&format!("{}/v1", config.api_url())).map_err(|e| {
        SecretError::resolution_failed(name, format!("Invalid GCP Secret Manager API URL: {e}"))
    })?;
    let version_access = format!("{}:access", config.version);

    url.path_segments_mut()
        .map_err(|()| {
            SecretError::resolution_failed(name, "GCP Secret Manager API URL cannot be a base")
        })?
        .extend([
            "projects",
            &config.project,
            "secrets",
            &config.secret,
            "versions",
            &version_access,
        ]);

    Ok(url)
}

fn default_version() -> String {
    "latest".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error;
    use std::io;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::Mutex as TestMutex;

    static TEST_ENV_LOCK: TestMutex<()> = TestMutex::const_new(());

    struct MockResponse {
        expected_path: &'static str,
        expected_auth: &'static str,
        status: u16,
        body: &'static str,
    }

    #[test]
    fn config_defaults() {
        let config = GcpSecretConfig::new("project", "api-key");

        assert_eq!(config.project, "project");
        assert_eq!(config.secret, "api-key");
        assert_eq!(config.version, "latest");
        assert!(config.api_url.is_none());
    }

    #[test]
    fn config_deserializes_cue_shape() -> Result<(), Box<dyn Error>> {
        let json = r#"{
            "resolver": "gcp",
            "project": "my-project",
            "secret": "DATABASE_URL",
            "version": "42",
            "_ref": "projects/my-project/secrets/DATABASE_URL/versions/42"
        }"#;

        let config: GcpSecretConfig = serde_json::from_str(json)?;

        assert_eq!(config.project, "my-project");
        assert_eq!(config.secret, "DATABASE_URL");
        assert_eq!(config.version, "42");
        Ok(())
    }

    #[test]
    fn secret_url_contains_expected_path() -> Result<(), Box<dyn Error>> {
        let mut config = GcpSecretConfig::new("project id", "API KEY");
        config.version = "7".to_string();
        config.api_url = Some("https://example.com/".to_string());

        let url = secret_url("API KEY", &config)?;

        assert_eq!(
            url.as_str(),
            "https://example.com/v1/projects/project%20id/secrets/API%20KEY/versions/7:access"
        );
        Ok(())
    }

    #[tokio::test]
    async fn resolves_with_direct_token() -> Result<(), Box<dyn Error>> {
        let _guard = TEST_ENV_LOCK.lock().await;
        let (api_url, server) = spawn_server(MockResponse {
            expected_path: "GET /v1/projects/project/secrets/API_KEY/versions/latest:access",
            expected_auth: "Bearer direct-token",
            status: 200,
            body: r#"{"payload":{"data":"ZGlyZWN0LXNlY3JldA=="}}"#,
        })
        .await?;

        temp_env::async_with_vars(
            [
                ("GOOGLE_OAUTH_ACCESS_TOKEN", Some("direct-token")),
                ("GOOGLE_APPLICATION_CREDENTIALS", None),
            ],
            async {
                let resolver = GcpSecretManagerResolver::new()?;
                let mut config = GcpSecretConfig::new("project", "API_KEY");
                config.api_url = Some(api_url);
                let spec = SecretSpec::new(serde_json::to_string(&config)?);
                let value = resolver.resolve("API_KEY", &spec).await?;
                Ok::<_, Box<dyn Error>>(value)
            },
        )
        .await
        .map(|value| assert_eq!(value, "direct-secret"))?;

        server.await??;
        Ok(())
    }

    async fn spawn_server(
        response: MockResponse,
    ) -> Result<(String, tokio::task::JoinHandle<io::Result<()>>), Box<dyn Error>> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let handle = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await?;
            let mut buffer = vec![0_u8; 8192];
            let read = stream.read(&mut buffer).await?;
            let request = String::from_utf8_lossy(&buffer[..read]);
            if !request.contains(response.expected_path) {
                return Err(io::Error::other(format!(
                    "request did not contain expected path: {}",
                    response.expected_path
                )));
            }
            if !request.contains(response.expected_auth) {
                return Err(io::Error::other("request did not contain expected auth"));
            }

            let body = response.body;
            let http_response = format!(
                "HTTP/1.1 {} OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                response.status,
                body.len(),
                body
            );
            stream.write_all(http_response.as_bytes()).await?;
            Ok(())
        });

        Ok((format!("http://{addr}"), handle))
    }
}
