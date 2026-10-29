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
            return Err(SecretError::resolution_failed(
                name,
                "Infisical projectId cannot be empty",
            ));
        }
        if self.environment.trim().is_empty() {
            return Err(SecretError::resolution_failed(
                name,
                "Infisical environment cannot be empty",
            ));
        }
        if self.secret_name.trim().is_empty() {
            return Err(SecretError::resolution_failed(
                name,
                "Infisical secretName cannot be empty",
            ));
        }
        if self.secret_type != "shared" && self.secret_type != "personal" {
            return Err(SecretError::resolution_failed(
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

/// Resolves secrets from Infisical using Universal Auth, OIDC Auth, or direct
/// access tokens.
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
    pub fn try_new() -> Result<Self, SecretError> {
        Ok(Self {
            client: build_client("infisical")?,
            cached_token: Mutex::new(None),
        })
    }

    fn parse_config(name: &str, spec: &SecretSpec) -> Result<InfisicalConfig, SecretError> {
        let config: InfisicalConfig = serde_json::from_str(&spec.source).map_err(|e| {
            SecretError::resolution_failed(
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
            AuthMode::Login(credentials) => {
                let access_token = self.login_access_token(name, &credentials).await?;
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

    async fn login_access_token(
        &self,
        name: &str,
        credentials: &LoginCredentials,
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
        // The access token is as powerful as the credentials that minted it.
        cuenv_events::register_secret(access_token.clone());
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
        credentials: &LoginCredentials,
    ) -> Result<LoginResponse, SecretError> {
        let (path, request, method) = match &credentials.method {
            LoginMethod::Universal {
                client_id,
                client_secret,
            } => (
                "/api/v1/auth/universal-auth/login",
                LoginRequest::Universal {
                    client_id,
                    client_secret,
                    organization_slug: credentials.organization_slug.as_deref(),
                },
                "Universal Auth",
            ),
            LoginMethod::Oidc { identity_id, jwt } => (
                "/api/v1/auth/oidc-auth/login",
                LoginRequest::Oidc {
                    identity_id,
                    jwt,
                    organization_slug: credentials.organization_slug.as_deref(),
                },
                "OIDC Auth",
            ),
        };
        let url = endpoint_url(&credentials.api_url, path, name)?;

        let response = self
            .client
            .post(url)
            .json(&request)
            .send()
            .await
            .map_err(|e| {
                SecretError::resolution_failed(name, format!("Infisical auth request failed: {e}"))
            })?;

        let status = response.status();
        if !status.is_success() {
            let hint = match credentials.method {
                LoginMethod::Universal { .. } => "",
                LoginMethod::Oidc { .. } => {
                    " (check that INFISICAL_AUTH_JWT has not expired and that its issuer, \
                     subject and audience match the identity's OIDC Auth configuration)"
                }
            };
            return Err(SecretError::resolution_failed(
                name,
                format!("Infisical {method} login failed with HTTP {status}{hint}"),
            ));
        }

        response.json::<LoginResponse>().await.map_err(|e| {
            SecretError::resolution_failed(
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
                SecretError::resolution_failed(
                    &config.secret_name,
                    format!("Infisical read failed: {e}"),
                )
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
                format!("Infisical secret read failed with HTTP {status}"),
            ));
        }

        let body = response.json::<SecretReadResponse>().await.map_err(|e| {
            SecretError::resolution_failed(
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
    Login(LoginCredentials),
    Token(String),
}

impl AuthMode {
    fn from_env(name: &str, config: &InfisicalConfig) -> Result<Self, SecretError> {
        let method = InfisicalAuthMethod::detect(env_var)
            .map_err(|e| SecretError::resolution_failed(name, e.to_string()))?;
        let organization_slug = env_var("INFISICAL_ORGANIZATION_SLUG");
        let login = |method| {
            Self::Login(LoginCredentials {
                api_url: config.api_url(),
                method,
                organization_slug,
            })
        };
        Ok(match method {
            InfisicalAuthMethod::Universal => login(LoginMethod::Universal {
                client_id: required_env("INFISICAL_CLIENT_ID"),
                client_secret: required_env("INFISICAL_CLIENT_SECRET"),
            }),
            InfisicalAuthMethod::Oidc => login(LoginMethod::Oidc {
                identity_id: required_env("INFISICAL_MACHINE_IDENTITY_ID"),
                jwt: required_env("INFISICAL_AUTH_JWT"),
            }),
            InfisicalAuthMethod::Token => Self::Token(required_env("INFISICAL_TOKEN")),
        })
    }
}

/// Read a variable that `InfisicalAuthMethod::detect` has already seen set.
fn required_env(name: &str) -> String {
    env_var(name).unwrap_or_default()
}

/// How cuenv authenticates to Infisical, chosen from the environment.
///
/// Precedence, applied by [`InfisicalAuthMethod::detect`]:
///
/// 1. A partial login configuration is an error: `INFISICAL_CLIENT_ID`
///    without `INFISICAL_CLIENT_SECRET` (or the reverse), or
///    `INFISICAL_MACHINE_IDENTITY_ID` without `INFISICAL_AUTH_JWT` (or the
///    reverse).
/// 2. Complete Universal Auth and complete OIDC Auth together is an error:
///    cuenv does not guess which identity was meant.
/// 3. Complete Universal Auth or complete OIDC Auth logs in; `INFISICAL_TOKEN`
///    is then ignored.
/// 4. Otherwise `INFISICAL_TOKEN` is used as the access token.
/// 5. With none of these set, resolution fails.
///
/// The resolver never fetches the JWT itself: whatever runs cuenv (a CI
/// contributor, a script) puts it in `INFISICAL_AUTH_JWT`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InfisicalAuthMethod {
    /// Universal Auth: `INFISICAL_CLIENT_ID` and `INFISICAL_CLIENT_SECRET`.
    Universal,
    /// OIDC Auth: `INFISICAL_MACHINE_IDENTITY_ID` and `INFISICAL_AUTH_JWT`.
    Oidc,
    /// A ready access token in `INFISICAL_TOKEN`.
    Token,
}

/// Why the environment does not select exactly one Infisical auth method.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InfisicalAuthConfigError {
    /// Only one of the two Universal Auth variables is set.
    PartialUniversal,
    /// Only one of the two OIDC Auth variables is set.
    PartialOidc {
        /// Whether the identity ID is the one that is set.
        has_identity_id: bool,
    },
    /// Both Universal Auth and OIDC Auth are fully configured.
    Ambiguous,
    /// No credentials at all.
    Missing,
}

impl std::fmt::Display for InfisicalAuthConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PartialUniversal => {
                f.write_str("INFISICAL_CLIENT_ID and INFISICAL_CLIENT_SECRET must be set together")
            }
            Self::PartialOidc {
                has_identity_id: true,
            } => f.write_str(
                "INFISICAL_MACHINE_IDENTITY_ID is set but INFISICAL_AUTH_JWT is not; \
                 OIDC Auth needs both (on GitHub Actions, add the #InfisicalOidc contributor, \
                 which grants id-token: write and exports the job's ID token)",
            ),
            Self::PartialOidc {
                has_identity_id: false,
            } => f.write_str(
                "INFISICAL_AUTH_JWT is set but INFISICAL_MACHINE_IDENTITY_ID is not; \
                 OIDC Auth needs both",
            ),
            Self::Ambiguous => f.write_str(
                "both Universal Auth (INFISICAL_CLIENT_ID, INFISICAL_CLIENT_SECRET) and \
                 OIDC Auth (INFISICAL_MACHINE_IDENTITY_ID, INFISICAL_AUTH_JWT) are set; \
                 unset one of them",
            ),
            Self::Missing => f.write_str(
                "Set INFISICAL_CLIENT_ID and INFISICAL_CLIENT_SECRET (Universal Auth), \
                 INFISICAL_MACHINE_IDENTITY_ID and INFISICAL_AUTH_JWT (OIDC Auth), \
                 or INFISICAL_TOKEN",
            ),
        }
    }
}

impl std::error::Error for InfisicalAuthConfigError {}

impl InfisicalAuthMethod {
    /// Choose the auth method from environment variables read through
    /// `lookup`, which returns `None` for unset or blank variables.
    ///
    /// # Errors
    ///
    /// Returns an error for partial, ambiguous or missing configuration; see
    /// the type documentation for the precedence rules.
    pub fn detect(
        lookup: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, InfisicalAuthConfigError> {
        let has = |name: &str| lookup(name).is_some();
        let universal = match (has("INFISICAL_CLIENT_ID"), has("INFISICAL_CLIENT_SECRET")) {
            (true, true) => true,
            (false, false) => false,
            _ => return Err(InfisicalAuthConfigError::PartialUniversal),
        };
        let oidc = match (
            has("INFISICAL_MACHINE_IDENTITY_ID"),
            has("INFISICAL_AUTH_JWT"),
        ) {
            (true, true) => true,
            (false, false) => false,
            (has_identity_id, _) => {
                return Err(InfisicalAuthConfigError::PartialOidc { has_identity_id });
            }
        };
        match (universal, oidc) {
            (true, true) => Err(InfisicalAuthConfigError::Ambiguous),
            (true, false) => Ok(Self::Universal),
            (false, true) => Ok(Self::Oidc),
            (false, false) if has("INFISICAL_TOKEN") => Ok(Self::Token),
            (false, false) => Err(InfisicalAuthConfigError::Missing),
        }
    }

    /// Choose the auth method from the process environment.
    ///
    /// # Errors
    ///
    /// See [`InfisicalAuthMethod::detect`].
    pub fn from_process_env() -> Result<Self, InfisicalAuthConfigError> {
        Self::detect(env_var)
    }
}

struct LoginCredentials {
    api_url: String,
    method: LoginMethod,
    organization_slug: Option<String>,
}

enum LoginMethod {
    Universal {
        client_id: String,
        client_secret: String,
    },
    Oidc {
        identity_id: String,
        jwt: String,
    },
}

impl LoginCredentials {
    fn cache_key(&self) -> TokenCacheKey {
        let principal = match &self.method {
            LoginMethod::Universal { client_id, .. } => {
                TokenPrincipal::Universal(client_id.clone())
            }
            LoginMethod::Oidc { identity_id, .. } => TokenPrincipal::Oidc(identity_id.clone()),
        };
        TokenCacheKey {
            api_url: self.api_url.clone(),
            principal,
            organization_slug: self.organization_slug.clone(),
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
enum TokenPrincipal {
    Universal(String),
    Oidc(String),
}

#[derive(Clone, PartialEq, Eq)]
struct TokenCacheKey {
    api_url: String,
    principal: TokenPrincipal,
    organization_slug: Option<String>,
}

struct CachedToken {
    key: TokenCacheKey,
    access_token: String,
    expires_at: Instant,
}

#[derive(Serialize)]
#[serde(untagged)]
enum LoginRequest<'a> {
    #[serde(rename_all = "camelCase")]
    Universal {
        client_id: &'a str,
        client_secret: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        organization_slug: Option<&'a str>,
    },
    #[serde(rename_all = "camelCase")]
    Oidc {
        identity_id: &'a str,
        jwt: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        organization_slug: Option<&'a str>,
    },
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
        .map_err(|()| {
            SecretError::resolution_failed(
                &config.secret_name,
                "Infisical API URL cannot be a base",
            )
        })?
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
    Url::parse(&format!("{api_url}{path}")).map_err(|e| {
        SecretError::resolution_failed(name, format!("Invalid Infisical API URL: {e}"))
    })
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

        temp_env::async_with_vars(auth_env(&[("INFISICAL_TOKEN", "direct-token")]), async {
            let resolver = InfisicalResolver::try_new()?;
            let mut config = InfisicalConfig::new("project", "prod", "API_KEY");
            config.api_url = Some(api_url);
            let spec = SecretSpec::new(serde_json::to_string(&config)?);
            let value = resolver.resolve("API_KEY", &spec).await?;
            Ok::<_, Box<dyn Error>>(value)
        })
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
            auth_env(&[
                ("INFISICAL_CLIENT_ID", "client-id"),
                ("INFISICAL_CLIENT_SECRET", "client-secret"),
                ("INFISICAL_ORGANIZATION_SLUG", "org"),
                ("INFISICAL_TOKEN", "fallback-token"),
            ]),
            async {
                let resolver = InfisicalResolver::try_new()?;
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
            auth_env(&[
                ("INFISICAL_CLIENT_ID", "client-id"),
                ("INFISICAL_TOKEN", "fallback-token"),
            ]),
            async {
                let resolver = InfisicalResolver::try_new()?;
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

    /// Every variable `InfisicalAuthMethod::detect` reads, unset; tests
    /// override the ones they need so the host environment cannot leak in.
    const AUTH_VARS: [&str; 6] = [
        "INFISICAL_CLIENT_ID",
        "INFISICAL_CLIENT_SECRET",
        "INFISICAL_MACHINE_IDENTITY_ID",
        "INFISICAL_AUTH_JWT",
        "INFISICAL_TOKEN",
        "INFISICAL_ORGANIZATION_SLUG",
    ];

    fn auth_env<'a>(set: &[(&'a str, &'a str)]) -> Vec<(&'a str, Option<&'a str>)> {
        AUTH_VARS
            .iter()
            .map(|name| {
                let value = set
                    .iter()
                    .find(|(set_name, _)| set_name == name)
                    .map(|(_, value)| *value);
                (*name, value)
            })
            .collect()
    }

    fn detect_with(set: &[(&str, &str)]) -> Result<InfisicalAuthMethod, InfisicalAuthConfigError> {
        InfisicalAuthMethod::detect(|name| {
            set.iter()
                .find(|(set_name, _)| *set_name == name)
                .map(|(_, value)| (*value).to_string())
        })
    }

    #[test]
    fn auth_method_precedence() {
        use InfisicalAuthConfigError as E;
        use InfisicalAuthMethod as M;
        let universal = [
            ("INFISICAL_CLIENT_ID", "id"),
            ("INFISICAL_CLIENT_SECRET", "secret"),
        ];
        let oidc = [
            ("INFISICAL_MACHINE_IDENTITY_ID", "identity"),
            ("INFISICAL_AUTH_JWT", "jwt"),
        ];
        let token = [("INFISICAL_TOKEN", "token")];

        assert_eq!(detect_with(&universal), Ok(M::Universal));
        assert_eq!(detect_with(&oidc), Ok(M::Oidc));
        assert_eq!(detect_with(&token), Ok(M::Token));
        assert_eq!(
            detect_with(&[universal.as_slice(), &token].concat()),
            Ok(M::Universal)
        );
        assert_eq!(
            detect_with(&[oidc.as_slice(), &token].concat()),
            Ok(M::Oidc)
        );
        assert_eq!(
            detect_with(&[universal.as_slice(), &oidc].concat()),
            Err(E::Ambiguous)
        );
        assert_eq!(detect_with(&[]), Err(E::Missing));
    }

    #[test]
    fn partial_auth_configuration_is_an_error_even_with_a_fallback() {
        use InfisicalAuthConfigError as E;
        let token = ("INFISICAL_TOKEN", "token");
        assert_eq!(
            detect_with(&[("INFISICAL_MACHINE_IDENTITY_ID", "identity"), token]),
            Err(E::PartialOidc {
                has_identity_id: true
            })
        );
        assert_eq!(
            detect_with(&[("INFISICAL_AUTH_JWT", "jwt"), token]),
            Err(E::PartialOidc {
                has_identity_id: false
            })
        );
        // A partial OIDC configuration is not hidden by complete Universal Auth.
        assert_eq!(
            detect_with(&[
                ("INFISICAL_CLIENT_ID", "id"),
                ("INFISICAL_CLIENT_SECRET", "secret"),
                ("INFISICAL_AUTH_JWT", "jwt"),
            ]),
            Err(E::PartialOidc {
                has_identity_id: false
            })
        );
        assert_eq!(
            detect_with(&[("INFISICAL_CLIENT_SECRET", "secret"), token]),
            Err(E::PartialUniversal)
        );
    }

    #[test]
    fn oidc_login_request_matches_the_infisical_api() -> Result<(), Box<dyn Error>> {
        let request = LoginRequest::Oidc {
            identity_id: "identity",
            jwt: "header.payload.signature",
            organization_slug: None,
        };
        assert_eq!(
            serde_json::to_value(&request)?,
            serde_json::json!({"identityId": "identity", "jwt": "header.payload.signature"})
        );
        let request = LoginRequest::Oidc {
            identity_id: "identity",
            jwt: "jwt",
            organization_slug: Some("org"),
        };
        assert_eq!(
            serde_json::to_value(&request)?,
            serde_json::json!({"identityId": "identity", "jwt": "jwt", "organizationSlug": "org"})
        );
        Ok(())
    }

    async fn resolve_api_key(api_url: Option<String>) -> Result<String, Box<dyn Error>> {
        let resolver = InfisicalResolver::try_new()?;
        let mut config = InfisicalConfig::new("project", "prod", "API_KEY");
        config.api_url = api_url;
        let spec = SecretSpec::new(serde_json::to_string(&config)?);
        Ok(resolver.resolve("API_KEY", &spec).await?)
    }

    #[tokio::test]
    async fn resolves_with_oidc_auth() -> Result<(), Box<dyn Error>> {
        let _guard = TEST_ENV_LOCK.lock().await;
        let (api_url, server) = spawn_server(vec![
            MockResponse {
                expected: "POST /api/v1/auth/oidc-auth/login",
                status: 200,
                body: r#"{"accessToken":"oidc-token","expiresIn":7200,"accessTokenMaxTTL":7200,"tokenType":"Bearer"}"#,
            },
            MockResponse {
                expected: "Bearer oidc-token",
                status: 200,
                body: r#"{"secret":{"secretValue":"oidc-secret"}}"#,
            },
        ])
        .await?;

        let value = temp_env::async_with_vars(
            auth_env(&[
                ("INFISICAL_MACHINE_IDENTITY_ID", "identity-id"),
                ("INFISICAL_AUTH_JWT", "github.id.token"),
                ("INFISICAL_TOKEN", "fallback-token"),
            ]),
            resolve_api_key(Some(api_url)),
        )
        .await?;
        assert_eq!(value, "oidc-secret");

        server.await??;
        Ok(())
    }

    #[tokio::test]
    async fn oidc_login_sends_identity_and_jwt() -> Result<(), Box<dyn Error>> {
        let _guard = TEST_ENV_LOCK.lock().await;
        let (api_url, server) = spawn_server(vec![
            MockResponse {
                expected: r#"{"identityId":"identity-id","jwt":"github.id.token","organizationSlug":"org"}"#,
                status: 200,
                body: r#"{"accessToken":"oidc-token","expiresIn":7200,"accessTokenMaxTTL":7200,"tokenType":"Bearer"}"#,
            },
            MockResponse {
                expected: "Bearer oidc-token",
                status: 200,
                body: r#"{"secret":{"secretValue":"oidc-secret"}}"#,
            },
        ])
        .await?;

        temp_env::async_with_vars(
            auth_env(&[
                ("INFISICAL_MACHINE_IDENTITY_ID", "identity-id"),
                ("INFISICAL_AUTH_JWT", "github.id.token"),
                ("INFISICAL_ORGANIZATION_SLUG", "org"),
            ]),
            resolve_api_key(Some(api_url)),
        )
        .await?;

        server.await??;
        Ok(())
    }

    #[tokio::test]
    async fn oidc_access_token_is_cached_across_secrets() -> Result<(), Box<dyn Error>> {
        let _guard = TEST_ENV_LOCK.lock().await;
        let (api_url, server) = spawn_server(vec![
            MockResponse {
                expected: "POST /api/v1/auth/oidc-auth/login",
                status: 200,
                body: r#"{"accessToken":"oidc-token","expiresIn":7200,"accessTokenMaxTTL":7200,"tokenType":"Bearer"}"#,
            },
            MockResponse {
                expected: "Bearer oidc-token",
                status: 200,
                body: r#"{"secret":{"secretValue":"first"}}"#,
            },
            MockResponse {
                expected: "Bearer oidc-token",
                status: 200,
                body: r#"{"secret":{"secretValue":"second"}}"#,
            },
        ])
        .await?;

        let values = temp_env::async_with_vars(
            auth_env(&[
                ("INFISICAL_MACHINE_IDENTITY_ID", "identity-id"),
                ("INFISICAL_AUTH_JWT", "github.id.token"),
            ]),
            async {
                let resolver = InfisicalResolver::try_new()?;
                let mut config = InfisicalConfig::new("project", "prod", "API_KEY");
                config.api_url = Some(api_url);
                let spec = SecretSpec::new(serde_json::to_string(&config)?);
                let first = resolver.resolve("API_KEY", &spec).await?;
                let second = resolver.resolve("API_KEY", &spec).await?;
                Ok::<_, Box<dyn Error>>((first, second))
            },
        )
        .await?;
        assert_eq!(values, ("first".to_string(), "second".to_string()));

        server.await??;
        Ok(())
    }

    #[tokio::test]
    async fn oidc_login_http_failure_is_reported() -> Result<(), Box<dyn Error>> {
        let _guard = TEST_ENV_LOCK.lock().await;
        let (api_url, server) = spawn_server(vec![MockResponse {
            expected: "POST /api/v1/auth/oidc-auth/login",
            status: 401,
            body: r#"{"message":"jwt expired"}"#,
        }])
        .await?;

        let result = temp_env::async_with_vars(
            auth_env(&[
                ("INFISICAL_MACHINE_IDENTITY_ID", "identity-id"),
                ("INFISICAL_AUTH_JWT", "expired.id.token"),
            ]),
            resolve_api_key(Some(api_url)),
        )
        .await;
        let message = result.err().map(|e| e.to_string()).unwrap_or_default();
        assert!(
            message.contains("OIDC Auth login failed with HTTP 401"),
            "{message}"
        );
        assert!(message.contains("INFISICAL_AUTH_JWT"), "{message}");
        assert!(!message.contains("expired.id.token"), "{message}");

        server.await??;
        Ok(())
    }

    #[tokio::test]
    async fn ambiguous_and_partial_oidc_env_fail_before_any_request() -> Result<(), Box<dyn Error>>
    {
        let _guard = TEST_ENV_LOCK.lock().await;
        for set in [
            vec![
                ("INFISICAL_CLIENT_ID", "client-id"),
                ("INFISICAL_CLIENT_SECRET", "client-secret"),
                ("INFISICAL_MACHINE_IDENTITY_ID", "identity-id"),
                ("INFISICAL_AUTH_JWT", "github.id.token"),
            ],
            vec![
                ("INFISICAL_MACHINE_IDENTITY_ID", "identity-id"),
                ("INFISICAL_TOKEN", "fallback-token"),
            ],
        ] {
            // No server: a request would fail with a connection error instead.
            let result = temp_env::async_with_vars(
                auth_env(&set),
                resolve_api_key(Some("http://127.0.0.1:9".to_string())),
            )
            .await;
            let message = result.err().map(|e| e.to_string()).unwrap_or_default();
            assert!(message.contains("INFISICAL_"), "{message}");
            assert!(!message.contains("request failed"), "{message}");
        }
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
                let request = read_request(&mut stream).await?;
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

    /// Read one request: the headers, then as many body bytes as
    /// `content-length` announces, however the client split its writes.
    async fn read_request(stream: &mut tokio::net::TcpStream) -> io::Result<String> {
        let mut request = Vec::new();
        let mut chunk = [0_u8; 4096];
        loop {
            let read = stream.read(&mut chunk).await?;
            if read == 0 {
                break;
            }
            request.extend_from_slice(&chunk[..read]);
            let text = String::from_utf8_lossy(&request);
            if let Some(header_end) = text.find("\r\n\r\n") {
                let content_length = text[..header_end]
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())
                            .flatten()
                    })
                    .unwrap_or(0);
                if request.len() >= header_end + 4 + content_length {
                    break;
                }
            }
        }
        Ok(String::from_utf8_lossy(&request).into_owned())
    }
}
