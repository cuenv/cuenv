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
// Any REAPI cache works — bazel-remote, buildbarn, BuildBuddy, NativeLink,
// EngFlow and Namespace all expose the same endpoint that Bazel's
// `--remote_cache` takes.
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

	// Whether this machine may upload. Reading is always allowed.
	//
	// Defaults to false, and should stay false until filesystem isolation
	// lands: a task can currently read files it did not declare, so an
	// entry it records may be wrong on another machine. Uploading is what
	// turns one machine's unsound entry into everyone's. Give this to a
	// trusted CI builder and leave developers read-only.
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
	bearerTokenEnv?: string

	// An arbitrary header, for a provider that names its own.
	header?: close({
		name!:     string
		valueEnv!: string
	})
})
