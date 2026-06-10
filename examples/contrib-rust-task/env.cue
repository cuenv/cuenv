package examples

import (
	"github.com/cuenv/cuenv/schema"
	xRust "github.com/cuenv/cuenv/contrib/rust"
)

schema.#Project & {
	name: "contrib-rust-task"

	tasks: {
		fmt: xRust.#Fmt & {
			args: ["fmt", "--all", "--", "--check"]
			inputs: ["Cargo.toml", "Cargo.lock", "crates/**"]
		}
	}
}
