package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
	name: "checkout-api"
}

let _t = tasks

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

tasks: {
	// The three children run in parallel.
	check: schema.#TaskGroup & {
		type:  "group"
		lint:  schema.#Task & {command: "npm", args: ["run", "lint"]}
		types: schema.#Task & {command: "npm", args: ["run", "typecheck"]}
		test:  schema.#Task & {command: "npm", args: ["test"]}
	}

	// Waits for `check`. Cached: only re-runs when its inputs change.
	build: schema.#Task & {
		command:   "npm"
		args:      ["run", "build"]
		dependsOn: [check]
		inputs:    ["src/**", "package.json"]
		outputs:   ["dist/**"]
		cache: mode: "read-write"
	}
}

ci: {
	// Explicit opt-in: no workflow is generated without a provider.
	providers: ["github"]

	pipelines: {
		// Every pull request: run the build (and `check` before it).
		pr: {
			tasks: [_t.build]
			when: pullRequest: true
		}

		// Pushes to main: same tasks, with production secrets resolved.
		release: {
			environment: "production"
			tasks:       [_t.build]
			when: branch: "main"
		}
	}
}
