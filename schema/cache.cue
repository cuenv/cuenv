package schema

// =============================================================================
// Cache configuration
// =============================================================================

// Project-level cache settings.
//
// Task-level `cache` (#TaskCachePolicy) decides *whether* a given task is
// cached. This decides *where* the cache lives.
#Cache: {
	// Remote cache to read through to. Omit for a local-only cache.
	remote?: #RemoteCache

	// Local cache settings.
	local?: #LocalCache
}

// Local on-disk cache settings.
#LocalCache: {
	// Default maximum size for `cuenv cache gc` (e.g., "20GiB", "5GB").
	maxSize?: string
}

// Transport protocol spoken by a remote cache server.
//
//   "grpc" — Bazel Remote Execution API v2 over gRPC (default, including for
//            http:// and https:// endpoints for backwards compatibility).
//   "http" — Bazel HTTP/REST remote cache layout (/cas/<sha256>, /ac/<sha256>).
#CacheProtocol: "grpc" | "http"

// A cache server speaking the Bazel Remote Execution API v2 or Bazel HTTP
// remote cache protocol.
//
// For gRPC, the endpoint must expose REAPI v2 ActionCache, CAS, Capabilities
// and ByteStream services with SHA-256 digests. Compatibility is verified
// during connection; provider-specific behavior may still vary.
#RemoteCache: {
	// Endpoint URL. `grpcs://` is TLS, `grpc://` is plaintext. A bare
	// host:port is rejected rather than guessed, because guessing wrong
	// would send credentials in the clear.
	//
	// Overridden by $CUENV_REMOTE_CACHE, so CI can inject an endpoint
	// without editing CUE.
	endpoint!: string & =~"^(grpc|grpcs|http|https)://"

	// Transport protocol. Defaults to "grpc" so existing http:// and https://
	// gRPC endpoints continue to work unchanged. Set to "http" to speak the
	// Bazel HTTP/REST cache layout (/cas/<sha256>, /ac/<sha256>).
	//
	// Overridden by $CUENV_REMOTE_CACHE_PROTOCOL.
	protocol?: #CacheProtocol | *"grpc"

	// REAPI instance name. Most single-tenant servers use the empty
	// string; multi-tenant providers use it to select a cache.
	//
	// Overridden by $CUENV_REMOTE_CACHE_INSTANCE.
	// For HTTP caches, when non-empty this is appended as a path prefix before
	// `/cas` and `/ac`; note that `bazel-remote` ignores HTTP instance names
	// for CAS and only namespaces ActionCache keys when configured, so a URL
	// prefix alone cannot establish tenant isolation.
	instance?: string | *""

	// Upload opt-in. Reading is always allowed.
	//
	// Defaults to false. When true, cuenv publishes Action, Command, logs,
	// outputs, and ActionResult via a dedicated post-run publisher only after
	// a task succeeds under Linux strict confinement with a verified read-only
	// runtime image, networking disabled, descendants reaped, outputs projected,
	// and no project secrets in the environment.
	//
	// Overridden by $CUENV_REMOTE_CACHE_UPLOAD.
	upload?: bool | *false

	// Credentials. Values are never written in CUE — only the name of the
	// environment variable holding them.
	auth?: #CacheAuth

	// CI trust-boundary settings for generated workflows (`cuenv sync ci`).
	ci?: #RemoteCacheCi
}

// Credential provisioning mode for generated CI workflows (`cuenv sync ci`).
//
//   "secrets" — (default) inject static CI secrets (`readSecret` / `writeSecret`)
//               gated by `writeEnvironment` (GitHub) or `writeQueue` (Buildkite).
//   "ambient" — credentials are supplied outside static CI secrets (for example
//               via a Bazel `credentialHelper` such as `bazel-credential-nsc` or
//               an OIDC step writing to `$GITHUB_ENV`). Preserves
//               `isolatedCacheDir` and protected-trigger upload gating without
//               requiring `readSecret`, `writeSecret`, or `writeEnvironment`.
#RemoteCacheCiAuthMode: "secrets" | "ambient"

// CI trust-boundary settings for remote cache credentials in generated
// workflows (`cuenv sync ci`).
//
// Enforces read-only vs read-write credential separation at the CI platform's
// credential-issuance boundary (GitHub Environments / Buildkite secret access
// policies) rather than relying on client-side flags alone.
#RemoteCacheCi: {
	// Credential provisioning mode for generated workflows.
	authMode?: #RemoteCacheCiAuthMode | *"secrets"

	// Secret name holding a read-only cache credential for untrusted triggers
	// (pull requests, forks). Omit to disable remote cache access on untrusted
	// jobs when `authMode` is "secrets".
	readSecret?: string

	// Secret name holding a read-write cache credential, exposed only to
	// explicitly authorized jobs on protected refs when `authMode` is "secrets".
	writeSecret?: string

	// GitHub Actions Environment name gating `writeSecret` to protected
	// branches or tags (e.g. "cache-write").
	writeEnvironment?: string

	// Buildkite agent queue restricted via Buildkite secret access policies to
	// protected branches that may access `writeSecret`.
	writeQueue?: string

	// Isolate persistent local cache directories across trust levels
	// (untrusted PR vs protected push) by setting distinct `CUENV_CACHE_DIR`
	// paths per trust level.
	isolatedCacheDir?: bool | *true
}

// How to authenticate to a remote cache: exactly one of a bearer token, a
// named header, or a Bazel-compatible credential helper. Each alternative
// forbids the other fields, so a project that sets one resolves to it; without
// that the alternatives stay ambiguous and the whole `auth` value cannot be
// exported.
#CacheAuth: {
	// Environment variable holding a bearer token. Sent as
	// `authorization: Bearer <token>`, which is what the hosted providers
	// issue.
	bearerTokenEnv!:   string
	header?:           _|_
	credentialHelper?: _|_
} | {
	// An arbitrary header, for a provider that names its own.
	header!: {
		name!:     string
		valueEnv!: string
	}
	bearerTokenEnv?:   _|_
	credentialHelper?: _|_
} | {
	// Path or command name of a Bazel-compatible credential helper
	// (e.g. "bazel-credential-nsc"). Invoked as `<helper> get` with
	// `{"uri": "<endpoint>"}` on stdin; expects `{"headers": {...}}` on stdout.
	credentialHelper!: string & !=""
	bearerTokenEnv?:   _|_
	header?:           _|_
}
