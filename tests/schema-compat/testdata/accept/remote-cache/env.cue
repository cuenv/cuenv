// Cache settings with explicit HTTP protocol, local size budget, and bearer
// auth must unify cleanly with the embedded schema.#Project form.
package compat

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "accept"

cache: {
	local: maxSize: "10GB"
	remote: {
		endpoint: "https://cache.example.com/bazel"
		protocol: "http"
		instance: "team/ci"
		upload:   true
		auth: bearerTokenEnv: "CUENV_CACHE_TOKEN"
		ci: {
			readSecret:       "CACHE_RO_TOKEN"
			writeSecret:      "CACHE_RW_TOKEN"
			writeEnvironment: "cache-write"
			writeQueue:       "cache-write"
			isolatedCacheDir: true
		}
	}
}
