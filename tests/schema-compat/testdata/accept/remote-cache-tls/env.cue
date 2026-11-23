// Remote cache configured with custom CA and mTLS client certificate paths
// must unify cleanly with the embedded schema.#Project form.
package compat

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "accept"

cache: remote: {
	endpoint: "grpcs://cache.namespace.so:443"
	upload:   true
	tls: {
		caCertPath:     "/etc/ssl/custom-ca.pem"
		clientCertPath: "/run/mtls/client.crt"
		clientKeyPath:  "/run/mtls/client.key"
	}
	ci: {
		authMode:         "ambient"
		isolatedCacheDir: true
	}
}
