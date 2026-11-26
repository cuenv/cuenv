package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
	name: "checkout-api"
}

env: {
	NODE_ENV: "development" | "staging" | "production" | *"development"

	HOST: "127.0.0.1"
	PORT: "8080"
	URL:  "http://\(HOST):\(PORT)"

	// Per-environment overrides. Pick one with `cuenv exec -e production`.
	environment: {
		staging: {
			NODE_ENV: "staging"
			HOST:     "staging.checkout.example.com"
			PORT:     "443"
			URL:      "https://staging.checkout.example.com"
		}
		production: {
			NODE_ENV: "production"
			HOST:     "checkout.example.com"
			PORT:     "443"
			URL:      "https://checkout.example.com"
		}
	}
}
