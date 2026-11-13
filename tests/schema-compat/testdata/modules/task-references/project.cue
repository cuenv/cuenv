package cuenv

import "github.com/cuenv/cuenv/schema"

// A base at language v0.18.0 is written open: embedding schema.#Base closes
// the package, and every project below would fail on its `name`.
schema.#Base...

env: SHARED: "root"
