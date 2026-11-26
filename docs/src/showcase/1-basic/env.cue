package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
	name: "checkout-api"
}

env: {
	// Only these values are accepted. A typo like "prodction" fails
	// evaluation instead of surfacing at 2am.
	NODE_ENV: "development" | "staging" | "production" | *"development"

	HOST: "127.0.0.1"
	PORT: "8080"

	// Interpolation: derived from the values above.
	URL: "http://\(HOST):\(PORT)"
}
