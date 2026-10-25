// want: nmae: field not allowed
// min-language: v0.18.0
// Before min-language this top-level typo is accepted. The compat check requires that.
package compat

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "reject"

nmae: "typo"
