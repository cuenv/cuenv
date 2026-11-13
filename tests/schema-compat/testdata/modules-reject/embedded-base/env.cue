package cuenv

import "github.com/cuenv/cuenv/schema"

// At language v0.18.0 an embedded schema.#Base closes the package, so the
// project below cannot add its `name`. Write `schema.#Base...` instead.
schema.#Base

env: SHARED: "root"
