package rust

import "github.com/cuenv/cuenv/schema"

// #Rust provides a collection of reusable tasks for Rust projects.
// It includes standard commands for building, testing, and linting.
//
// Usage:
//
// import "github.com/cuenv/cuenv/contrib/rust"
//
// tasks: {
//   build: rust.#Build & {
//     inputs: ["Cargo.toml", "src"]
//   }
// }

// Common inputs used across most Rust tasks
// Users should override this to match their project structure
#BaseInputs: [...string] | *["Cargo.toml", "Cargo.lock", "src"]

// #Build runs 'cargo build'
#Build: schema.#Command & {
	command: "cargo"
	args: ["build", ...string]
	inputs: #BaseInputs
}

// #Test runs 'cargo test'
#Test: schema.#Command & {
	command: "cargo"
	args: ["test", ...string]
	inputs: #BaseInputs
}

// #Clippy runs 'cargo clippy'
#Clippy: schema.#Command & {
	command: "cargo"
	args: ["clippy", ...string]
	inputs: #BaseInputs
}

// #Fmt runs 'cargo fmt'
#Fmt: schema.#Command & {
	command: "cargo"
	args: ["fmt", ...string]
	inputs: #BaseInputs
}

// #Check runs 'cargo check' (faster than build)
#Check: schema.#Command & {
	command: "cargo"
	args: ["check", ...string]
	inputs: #BaseInputs
}

// #Doc runs 'cargo doc'
#Doc: schema.#Command & {
	command: "cargo"
	args: ["doc", ...string]
	inputs: #BaseInputs
}

// Default tasks exposed by this module
// Note: fmt and test are not exposed to avoid conflicts with common
// monorepo setups (treefmt, test groups).
build:  #Build
check:  #Check
clippy: #Clippy
doc:    #Doc
