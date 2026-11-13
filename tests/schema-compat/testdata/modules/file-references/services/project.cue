package cuenv

import "github.com/cuenv/cuenv/schema"

// A nested base, merged into every project below `services/`. A base has no
// #Files: files beside it are module files.
schema.#Base...

env: TIER: "services"
