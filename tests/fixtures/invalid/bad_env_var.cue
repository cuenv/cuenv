package cuenv

import "github.com/cuenv/cuenv/schema"

// Invalid: environment variable name doesn't match pattern
schema.#Cuenv & {
	env: {
		"lowercase_var": "invalid"  // Should start with uppercase
		"123_NUMBER": "invalid"     // Should start with letter
		"VALID_VAR": "ok"
	}
}