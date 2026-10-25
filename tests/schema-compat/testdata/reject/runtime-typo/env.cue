// want: flakee: field not allowed
package compat

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "reject"

runtime: {type: "nix", flakee: "."}
