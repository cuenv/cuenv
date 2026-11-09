// want: env.environment.production."9BAR": field not allowed
package compat

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "reject"

env: environment: production: "9BAR": "value"
