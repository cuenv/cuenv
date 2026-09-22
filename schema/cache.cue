package schema

// =============================================================================
// Cache configuration
// =============================================================================

// Project-level cache settings.
//
// Task-level `cache` (#TaskCachePolicy) decides *whether* a given task is
// cached. This decides *where* the cache lives.
#Cache: close({
	// Remote cache to read through to. Omit for a local-only cache.
	remote?: #RemoteCache
})

// A cache server speaking the Bazel Remote Execution API v2.
//
// The endpoint must expose REAPI v2 ActionCache, CAS, Capabilities and
// ByteStream services with SHA-256 digests. Compatibility is verified during
// connection; provider-specific behavior may still vary.
#RemoteCache: close({
	// Endpoint URL. `grpcs://` is TLS, `grpc://` is plaintext. A bare
	// host:port is rejected rather than guessed, because guessing wrong
	// would send credentials in the clear.
	//
	// Overridden by $CUENV_REMOTE_CACHE, so CI can inject an endpoint
	// without editing CUE.
	endpoint!: string & =~"^(grpc|grpcs|http|https)://"

	// REAPI instance name. Most single-tenant servers use the empty
	// string; multi-tenant providers use it to select a cache.
	instance?: string | *""

	// Reserved upload opt-in. Reading is always allowed.
	//
	// Defaults to false. cuenv currently forces remote connections read-only
	// even when this is true: directory execution roots isolate relative
	// workspace access but do not yet confine absolute host filesystem reads.
	// Upload will be enabled only after a strict platform sandbox lands.
	//
	// Overridden by $CUENV_REMOTE_CACHE_UPLOAD.
	upload?: bool | *false

	// Credentials. Values are never written in CUE — only the name of the
	// environment variable holding them.
	auth?: #CacheAuth
})

// How to authenticate to a remote cache.
#CacheAuth: close({
	// Environment variable holding a bearer token. Sent as
	// `authorization: Bearer <token>`, which is what the hosted providers
	// issue.
	bearerTokenEnv!: string
}) | close({
	// An arbitrary header, for a provider that names its own.
	header!: close({
		name!:     string
		valueEnv!: string
	})
})
