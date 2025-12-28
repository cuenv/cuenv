package rust

import "github.com/cuenv/cuenv/schema"

// #Rust provides Homebrew's Rust toolchain (cargo, rustc, clippy, rustfmt, rustdoc)
#Rust: schema.#Tool & {
	version!: string
	source: schema.#Homebrew & {formula: "rust"}
}

// #RustAnalyzer provides the rust-analyzer LSP server
#RustAnalyzer: schema.#Tool & {
	version!: string
	source: schema.#Homebrew & {formula: "rust-analyzer"}
}

// #CargoNextest provides the nextest test runner
#CargoNextest: schema.#Tool & {
	version!: string
	source: schema.#Homebrew & {formula: "cargo-nextest"}
}

// #CargoDeny provides license and security checking
#CargoDeny: schema.#Tool & {
	version!: string
	source: schema.#Homebrew & {formula: "cargo-deny"}
}

// #CargoLlvmCov provides code coverage via LLVM
#CargoLlvmCov: schema.#Tool & {
	version!: string
	source: schema.#Homebrew & {formula: "cargo-llvm-cov"}
}

// #CargoCyclonedx generates SBOM files
#CargoCyclonedx: schema.#Tool & {
	version!: string
	source: schema.#Homebrew & {formula: "cargo-cyclonedx"}
}

// #CargoZigbuild enables cross-compilation using Zig
#CargoZigbuild: schema.#Tool & {
	version!: string
	source: schema.#Homebrew & {formula: "cargo-zigbuild"}
}

// #SccacheTool provides compilation caching
#SccacheTool: schema.#Tool & {
	version!: string
	source: schema.#Homebrew & {formula: "sccache"}
}

// #Zig provides the Zig toolchain (required by cargo-zigbuild)
#Zig: schema.#Tool & {
	version!: string
	source: schema.#Homebrew & {formula: "zig"}
}
