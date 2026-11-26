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

	environment: {
		staging: {
			NODE_ENV: "staging"
			HOST:     "staging.checkout.example.com"
			PORT:     "443"
			URL:      "https://staging.checkout.example.com"

			DATABASE_PASSWORD: schema.#OnePasswordRef & {
				ref: "op://Engineering/checkout-staging-db/password"
			}
		}
		production: {
			NODE_ENV: "production"
			HOST:     "checkout.example.com"
			PORT:     "443"
			URL:      "https://checkout.example.com"

			// Resolved at runtime and redacted from output. The value never
			// lands in env.cue, generated files, or your shell history.
			DATABASE_PASSWORD: schema.#OnePasswordRef & {
				ref: "op://Engineering/checkout-prod-db/password"
			}
			STRIPE_API_KEY: schema.#AwsSecret & {
				secretId: "prod/stripe"
				jsonKey:  "api_key"
			}
		}
	}
}
