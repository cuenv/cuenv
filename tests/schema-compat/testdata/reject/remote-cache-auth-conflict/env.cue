// Setting both bearerTokenEnv and credentialHelper must be rejected by #CacheAuth.
// want: empty disjunction
package compat

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "reject"

cache: remote: {
	endpoint: "grpcs://cache.namespace.so:443"
	auth: {
		bearerTokenEnv:   "CUENV_CACHE_TOKEN"
		credentialHelper: "bazel-credential-nsc"
	}
}
