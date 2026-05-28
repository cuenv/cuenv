//! Infisical REST API secret resolver.

use async_trait::async_trait;
use cuenv_secrets::http::{build_client, env_var, normalize_api_url};
use cuenv_secrets::{SecretError, SecretResolver, SecretSpec};
use reqwest::{Client, StatusCode, Url, header};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

const DEFAULT_API_URL: &str = "https://us.infisical.com";
const TOKEN_REFRESH_SKEW_SECONDS: u64 = 60;

/// Configuration for resolving a single Infisical secret.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct InfisicalConfig {
    /// Infisical project ID.
    pub project_id: String,

    /// Infisical environment slug.
    pub environment: String,

    /// Secret name to read.
    pub secret_name: String,

    /// Secret folder path.
    #[serde(default = "default_secret_path")]
    pub secret_path: String,

    /// Infisical secret type.
    #[serde(rename = "type", default = "default_secret_type")]
    pub secret_type: String,

    /// Optional secret version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<u64>,

    /// Whether Infisical should expand secret references.
    #[serde(default = "default_true")]
    pub expand_secret_references: bool,

    /// Whether Infisical should include imported secrets.
    #[serde(default = "default_true")]
    pub include_imports: bool,

    /// Optional API base URL. Falls back to `INFISICAL_API_URL`, then Infisical US cloud.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_url: Option<String>,
}

impl InfisicalConfig {
    /// Create a new Infisical secret config.
    #[must_use]
    pub fn new(
        project_id: impl Into<String>,
        environment: impl Into<String>,
        secret_name: impl Into<String>,
    ) -> Self {
        Self {
            project_id: project_id.into(),
            environment: environment.into(),
            secret_name: secret_name.into(),
            secret_path: default_secret_path(),
            secret_type: default_secret_type(),
            version: None,
            expand_secret_references: true,
            include_imports: true,
            api_url: None,
        }
    }

    fn validate(&self, name: &str) -> Result<(), SecretError> {
        if self.project_id.trim().is_empty() {
            return Err(infisical_error(name, "Infisical projectId cannot be empty"));
        }
        if self.environment.trim().is_empty() {
            return Err(infisical_error(
                name,
                "Infisical environment cannot be empty",
            ));
        }
        if self.secret_name.trim().is_empty() {
            return Err(infisical_error(
                name,
                "Infisical secretName cannot be empty",
            ));
        }
        if self.secret_type != "shared" && self.secret_type != "personal" {
            return Err(infisical_error(
                name,
                "Infisical secret type must be either 'shared' or 'personal'",
            ));
        }
        Ok(())
    }

    fn api_url(&self) -> String {
        let configured = self
            .api_url
            .clone()
            .or_else(|| env_var("INFISICAL_API_URL"));
        normalize_api_url(configured.as_deref().unwrap_or(DEFAULT_API_URL))
    }
}

/// Resolves secrets from Infisical using Universal Auth or direct access tokens.
pub struct InfisicalResolver {
    client: Client,
    cached_token: Mutex<Option<CachedToken>>,
}

impl std::fmt::Debug for InfisicalResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InfisicalResolver").finish_non_exhaustive()
    }
}

impl InfisicalResolver {
    /// Create a new Infisical resolver.
    ///
    /// # Errors
    ///
    /// Returns an error if the HTTP client cannot be initialized.
    pub fn new() -> Result<Self, SecretError> {
        Ok(Self {
            client: build_client("infisical")?,
            cached_token: Mutex::new(None),
        })
    }

    fn parse_config(name: &str, spec: &SecretSpec) -> Result<InfisicalConfig, SecretError> {
        let config: InfisicalConfig = serde_json::from_str(&spec.source).map_err(|e| {
            infisical_error(
                name,
                format!("Infisical resolver requires structured config: {e}"),
            )
        })?;
        config.validate(name)?;
        Ok(config)
    }

    async fn authorization_header(
        &self,
        name: &str,
        config: &InfisicalConfig,
    ) -> Result<AuthHeader, SecretError> {
        match AuthMode::from_env(name, config)? {
            AuthMode::Universal(credentials) => {
                let access_token = self.universal_access_token(name, &credentials).await?;
                Ok(AuthHeader {
                    value: format!("Bearer {access_token}"),
                    refreshable: true,
                })
            }
            AuthMode::Token(token) => Ok(AuthHeader {
                value: format!("Bearer {token}"),
                refreshable: false,
            }),
        }
    }

    async fn universal_access_token(
        &self,
        name: &str,
        credentials: &UniversalCredentials,
    ) -> Result<String, SecretError> {
        let key = credentials.cache_key();
        let mut guard = self.cached_token.lock().await;

        if let Some(cached) = guard.as_ref()
            && cached.key == key
            && cached.expires_at > Instant::now()
        {
            return Ok(cached.access_token.clone());
        }

        let response = self.login(name, credentials).await?;
        let expires_at = token_expiry(response.expires_in);
        let access_token = response.access_token;
        *guard = Some(CachedToken {
            key,
            access_token: access_token.clone(),
            expires_at,
        });

        Ok(access_token)
    }

    async fn login(
        &self,
        name: &str,
        credentials: &UniversalCredentials,
    ) -> Result<LoginResponse, SecretError> {
        let url = endpoint_url(
            &credentials.api_url,
            "/api/v1/auth/universal-auth/login",
            name,
        )?;
        let request = LoginRequest {
            client_id: &credentials.client_id,
            client_secret: &credentials.client_secret,
            organization_slug: credentials.organization_slug.as_deref(),
        };

        let response = self
            .client
            .post(url)
            .json(&request)
            .send()
            .await
            .map_err(|e| infisical_error(name, format!("Infisical auth request failed: {e}")))?;

        let status = response.status();
        if !status.is_success() {
            return Err(infisical_error(
                name,
                format!("Infisical Universal Auth login failed with HTTP {status}"),
            ));
        }

        response.json::<LoginResponse>().await.map_err(|e| {
            infisical_error(
                name,
                format!("Failed to parse Infisical auth response: {e}"),
            )
        })
    }

    async fn clear_cached_token(&self) {
        let mut guard = self.cached_token.lock().await;
        *guard = None;
    }

    async fn send_secret_request(
        &self,
        config: &InfisicalConfig,
        auth: &AuthHeader,
    ) -> Result<reqwest::Response, SecretError> {
        let url = secret_url(config)?;
        self.client
            .get(url)
            .header(header::AUTHORIZATION, &auth.value)
            .send()
            .await
            .map_err(|e| {
                infisical_error(&config.secret_name, format!("Infisical read failed: {e}"))
            })
    }

    async fn parse_secret_response(
        name: &str,
        response: reqwest::Response,
    ) -> Result<String, SecretError> {
        let status = response.status();
        if !status.is_success() {
            return Err(infisical_error(
                name,
                format!("Infisical secret read failed with HTTP {status}"),
            ));
        }

        let body = response.json::<SecretReadResponse>().await.map_err(|e| {
            infisical_error(
                name,
                format!("Failed to parse Infisical secret response: {e}"),
            )
        })?;

        Ok(body.secret.secret_value)
    }
}

#[async_trait]
impl SecretResolver for InfisicalResolver {
    async fn resolve(&self, name: &str, spec: &SecretSpec) -> Result<String, SecretError> {
        let config = Self::parse_config(name, spec)?;
        let mut auth = self.authorization_header(name, &config).await?;
        let mut response = self.send_secret_request(&config, &auth).await?;

        if response.status() == StatusCode::UNAUTHORIZED && auth.refreshable {
            self.clear_cached_token().await;
            auth = self.authorization_header(name, &config).await?;
            response = self.send_secret_request(&config, &auth).await?;
        }

        Self::parse_secret_response(name, response).await
    }

    fn provider_name(&self) -> &'static str {
        "infisical"
    }
}

#[derive(Clone)]
struct AuthHeader {
    value: String,
    refreshable: bool,
}

enum AuthMode {
    Universal(UniversalCredentials),
    Token(String),
}

impl AuthMode {
    fn from_env(name: &str, config: &InfisicalConfig) -> Result<Self, SecretError> {
        let client_id = env_var("INFISICAL_CLIENT_ID");
        let client_secret = env_var("INFISICAL_CLIENT_SECRET");

        match (client_id, client_secret) {
            (Some(client_id), Some(client_secret)) => Ok(Self::Universal(UniversalCredentials {
                api_url: config.api_url(),
                client_id,
                client_secret,
                organization_slug: env_var("INFISICAL_ORGANIZATION_SLUG"),
            })),
            (None, None) => env_var("INFISICAL_TOKEN").map(Self::Token).ok_or_else(|| {
                infisical_error(
                    name,
                    "Set INFISICAL_CLIENT_ID and INFISICAL_CLIENT_SECRET, or set INFISICAL_TOKEN",
                )
            }),
            _ => Err(infisical_error(
                name,
                "INFISICAL_CLIENT_ID and INFISICAL_CLIENT_SECRET must be set together",
            )),
        }
    }
}

struct UniversalCredentials {
    api_url: String,
    client_id: String,
    client_secret: String,
    organization_slug: Option<String>,
}

impl UniversalCredentials {
    fn cache_key(&self) -> TokenCacheKey {
        TokenCacheKey {
            api_url: self.api_url.clone(),
            client_id: self.client_id.clone(),
            organization_slug: self.organization_slug.clone(),
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
struct TokenCacheKey {
    api_url: String,
    client_id: String,
    organization_slug: Option<String>,
}

struct CachedToken {
    key: TokenCacheKey,
    access_token: String,
    expires_at: Instant,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LoginRequest<'a> {
    client_id: &'a str,
    client_secret: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    organization_slug: Option<&'a str>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LoginResponse {
    access_token: String,
    expires_in: u64,
}

#[derive(Deserialize)]
struct SecretReadResponse {
    secret: SecretResponse,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SecretResponse {
    secret_value: String,
}

fn secret_url(config: &InfisicalConfig) -> Result<Url, SecretError> {
    let mut url = endpoint_url(&config.api_url(), "/api/v4/secrets", &config.secret_name)?;
    url.path_segments_mut()
        .map_err(|()| infisical_error(&config.secret_name, "Infisical API URL cannot be a base"))?
        .push(&config.secret_name);

    let mut query = url.query_pairs_mut();
    query.append_pair("projectId", &config.project_id);
    query.append_pair("environment", &config.environment);
    query.append_pair("secretPath", &config.secret_path);
    query.append_pair("type", &config.secret_type);
    query.append_pair("viewSecretValue", "true");
    query.append_pair(
        "expandSecretReferences",
        if config.expand_secret_references {
            "true"
        } else {
            "false"
        },
    );
    query.append_pair(
        "includeImports",
        if config.include_imports {
            "true"
        } else {
            "false"
        },
    );
    if let Some(version) = config.version {
        query.append_pair("version", &version.to_string());
    }
    drop(query);

    Ok(url)
}

fn endpoint_url(api_url: &str, path: &str, name: &str) -> Result<Url, SecretError> {
    Url::parse(&format!("{api_url}{path}"))
        .map_err(|e| infisical_error(name, format!("Invalid Infisical API URL: {e}")))
}

fn token_expiry(expires_in: u64) -> Instant {
    let refresh_after = expires_in.saturating_sub(TOKEN_REFRESH_SKEW_SECONDS);
    Instant::now() + Duration::from_secs(refresh_after)
}

fn default_secret_path() -> String {
    "/".to_string()
}

fn default_secret_type() -> String {
    "shared".to_string()
}

const fn default_true() -> bool {
    true
}

fn infisical_error(name: impl Into<String>, message: impl Into<String>) -> SecretError {
    SecretError::ResolutionFailed {
        name: name.into(),
        message: message.into(),
    }
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
        expected: &'static str,
        status: u16,
        body: &'static str,
    }

    #[test]
    fn config_defaults() {
        let config = InfisicalConfig::new("project", "dev", "API_KEY");

        assert_eq!(config.project_id, "project");
        assert_eq!(config.environment, "dev");
        assert_eq!(config.secret_name, "API_KEY");
        assert_eq!(config.secret_path, "/");
        assert_eq!(config.secret_type, "shared");
        assert!(config.expand_secret_references);
        assert!(config.include_imports);
    }

    #[test]
    fn config_deserializes_cue_shape() -> Result<(), Box<dyn Error>> {
        let json = r#"{
            "resolver": "infisical",
            "projectId": "project",
            "environment": "prod",
            "secretName": "DATABASE_URL",
            "secretPath": "/app",
            "type": "personal",
            "version": 3,
            "expandSecretReferences": false,
            "includeImports": false,
            "apiUrl": "https://example.com"
        }"#;

        let config: InfisicalConfig = serde_json::from_str(json)?;

        assert_eq!(config.project_id, "project");
        assert_eq!(config.environment, "prod");
        assert_eq!(config.secret_name, "DATABASE_URL");
        assert_eq!(config.secret_path, "/app");
        assert_eq!(config.secret_type, "personal");
        assert_eq!(config.version, Some(3));
        assert!(!config.expand_secret_references);
        assert!(!config.include_imports);
        assert_eq!(config.api_url.as_deref(), Some("https://example.com"));
        Ok(())
    }

    #[test]
    fn secret_url_contains_expected_query() -> Result<(), Box<dyn Error>> {
        let mut config = InfisicalConfig::new("project", "prod", "API KEY");
        config.secret_path = "/app".to_string();
        config.version = Some(7);
        config.api_url = Some("https://example.com/".to_string());

        let url = secret_url(&config)?;
        let rendered = url.as_str();

        assert!(rendered.starts_with("https://example.com/api/v4/secrets/API%20KEY?"));
        assert!(rendered.contains("projectId=project"));
        assert!(rendered.contains("environment=prod"));
        assert!(rendered.contains("secretPath=%2Fapp"));
        assert!(rendered.contains("version=7"));
        Ok(())
    }

    #[tokio::test]
    async fn resolves_with_direct_token() -> Result<(), Box<dyn Error>> {
        let _guard = TEST_ENV_LOCK.lock().await;
        let (api_url, server) = spawn_server(vec![MockResponse {
            expected: "Bearer direct-token",
            status: 200,
            body: r#"{"secret":{"secretValue":"direct-secret"}}"#,
        }])
        .await?;

        temp_env::async_with_vars(
            [
                ("INFISICAL_TOKEN", Some("direct-token")),
                ("INFISICAL_CLIENT_ID", None),
                ("INFISICAL_CLIENT_SECRET", None),
                ("INFISICAL_ORGANIZATION_SLUG", None),
            ],
            async {
                let resolver = InfisicalResolver::new()?;
                let mut config = InfisicalConfig::new("project", "prod", "API_KEY");
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

    #[tokio::test]
    async fn resolves_with_universal_auth() -> Result<(), Box<dyn Error>> {
        let _guard = TEST_ENV_LOCK.lock().await;
        let (api_url, server) = spawn_server(vec![
            MockResponse {
                expected: "POST /api/v1/auth/universal-auth/login",
                status: 200,
                body: r#"{"accessToken":"ua-token","expiresIn":7200,"accessTokenMaxTTL":7200,"tokenType":"Bearer"}"#,
            },
            MockResponse {
                expected: "Bearer ua-token",
                status: 200,
                body: r#"{"secret":{"secretValue":"ua-secret"}}"#,
            },
        ])
        .await?;

        temp_env::async_with_vars(
            [
                ("INFISICAL_CLIENT_ID", Some("client-id")),
                ("INFISICAL_CLIENT_SECRET", Some("client-secret")),
                ("INFISICAL_ORGANIZATION_SLUG", Some("org")),
                ("INFISICAL_TOKEN", Some("fallback-token")),
            ],
            async {
                let resolver = InfisicalResolver::new()?;
                let mut config = InfisicalConfig::new("project", "prod", "API_KEY");
                config.api_url = Some(api_url);
                let spec = SecretSpec::new(serde_json::to_string(&config)?);
                let value = resolver.resolve("API_KEY", &spec).await?;
                Ok::<_, Box<dyn Error>>(value)
            },
        )
        .await
        .map(|value| assert_eq!(value, "ua-secret"))?;

        server.await??;
        Ok(())
    }

    #[tokio::test]
    async fn partial_universal_auth_env_fails() -> Result<(), Box<dyn Error>> {
        let _guard = TEST_ENV_LOCK.lock().await;
        temp_env::async_with_vars(
            [
                ("INFISICAL_CLIENT_ID", Some("client-id")),
                ("INFISICAL_CLIENT_SECRET", None),
                ("INFISICAL_TOKEN", Some("fallback-token")),
            ],
            async {
                let resolver = InfisicalResolver::new()?;
                let config = InfisicalConfig::new("project", "prod", "API_KEY");
                let spec = SecretSpec::new(serde_json::to_string(&config)?);
                let result = resolver.resolve("API_KEY", &spec).await;
                assert!(matches!(result, Err(SecretError::ResolutionFailed { .. })));
                Ok::<_, Box<dyn Error>>(())
            },
        )
        .await?;
        Ok(())
    }

    async fn spawn_server(
        responses: Vec<MockResponse>,
    ) -> Result<(String, tokio::task::JoinHandle<io::Result<()>>), Box<dyn Error>> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let handle = tokio::spawn(async move {
            for response in responses {
                let (mut stream, _) = listener.accept().await?;
                let mut buffer = vec![0_u8; 8192];
                let read = stream.read(&mut buffer).await?;
                let request = String::from_utf8_lossy(&buffer[..read]);
                if !request.contains(response.expected) {
                    return Err(io::Error::other(format!(
                        "request did not contain expected text: {}",
                        response.expected
                    )));
                }

                let body = response.body;
                let http_response = format!(
                    "HTTP/1.1 {} OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    response.status,
                    body.len(),
                    body
                );
                stream.write_all(http_response.as_bytes()).await?;
            }
            Ok(())
        });

        Ok((format!("http://{addr}"), handle))
    }
}
