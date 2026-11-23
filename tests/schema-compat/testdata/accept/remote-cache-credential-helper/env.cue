// Remote cache configured with a Bazel credential helper and ambient CI mode
// must unify cleanly with the embedded schema.#Project form.
package compat

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "accept"

cache: remote: {
	endpoint: "grpcs://cache.namespace.so:443"
	upload:   true
	auth: credentialHelper: "bazel-credential-nsc"
	ci: {
		authMode:         "ambient"
		isolatedCacheDir: true
	}
}
