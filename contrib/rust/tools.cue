package rust

import "github.com/cuenv/cuenv/schema"

// #Rust provides the Rust toolchain via rustup (cargo, rustc, clippy, rustfmt, rustdoc).
//
// Usage:
//
//	runtime: schema.#ToolsRuntime & {
//	    tools: rust: xRust.#Rust & {
//	        version: "1.83.0"
//	        source: profile: "default"
//	        source: components: ["rust-src", "clippy", "rustfmt"]
//	        source: targets: ["x86_64-unknown-linux-gnu"]
//	    }
//	}
#Rust: schema.#Tool & {
	// The version is used as the toolchain identifier
	version!: string

	source: schema.#Rustup & {
		toolchain: version
	}
}

// #RustAnalyzer provides the rust-analyzer LSP server from GitHub releases.
// Note: rust-analyzer uses date-based tags (e.g., "2025-12-29"), not version numbers.
#RustAnalyzer: schema.#Tool & {
	version!: string
	overrides: [
		{os: "darwin", arch: "arm64", source: schema.#GitHub & {
			repo:  "rust-lang/rust-analyzer"
			asset: "rust-analyzer-aarch64-apple-darwin.gz"
		}},
		{os: "darwin", arch: "x86_64", source: schema.#GitHub & {
			repo:  "rust-lang/rust-analyzer"
			asset: "rust-analyzer-x86_64-apple-darwin.gz"
		}},
		{os: "linux", arch: "x86_64", source: schema.#GitHub & {
			repo:  "rust-lang/rust-analyzer"
			asset: "rust-analyzer-x86_64-unknown-linux-gnu.gz"
		}},
		{os: "linux", arch: "arm64", source: schema.#GitHub & {
			repo:  "rust-lang/rust-analyzer"
			asset: "rust-analyzer-aarch64-unknown-linux-gnu.gz"
		}},
	]
}

// #CargoNextest provides the nextest test runner from GitHub releases.
#CargoNextest: schema.#Tool & {
	version!: string
	overrides: [
		{os: "darwin", arch: "arm64", source: schema.#GitHub & {
			repo:  "nextest-rs/nextest"
			tag:   "cargo-nextest-{version}"
			asset: "cargo-nextest-{version}-universal-apple-darwin.tar.gz"
			path:  "cargo-nextest"
		}},
		{os: "darwin", arch: "x86_64", source: schema.#GitHub & {
			repo:  "nextest-rs/nextest"
			tag:   "cargo-nextest-{version}"
			asset: "cargo-nextest-{version}-universal-apple-darwin.tar.gz"
			path:  "cargo-nextest"
		}},
		{os: "linux", arch: "x86_64", source: schema.#GitHub & {
			repo:  "nextest-rs/nextest"
			tag:   "cargo-nextest-{version}"
			asset: "cargo-nextest-{version}-x86_64-unknown-linux-gnu.tar.gz"
			path:  "cargo-nextest"
		}},
		{os: "linux", arch: "arm64", source: schema.#GitHub & {
			repo:  "nextest-rs/nextest"
			tag:   "cargo-nextest-{version}"
			asset: "cargo-nextest-{version}-aarch64-unknown-linux-gnu.tar.gz"
			path:  "cargo-nextest"
		}},
	]
}

// #CargoDeny provides license and security checking from GitHub releases.
// Note: cargo-deny does NOT use v prefix in tags (uses "0.18.9" not "v0.18.9").
#CargoDeny: schema.#Tool & {
	version!: string
	overrides: [
		{os: "darwin", arch: "arm64", source: schema.#GitHub & {
			repo:  "EmbarkStudios/cargo-deny"
			asset: "cargo-deny-{version}-aarch64-apple-darwin.tar.gz"
			path:  "cargo-deny-{version}-aarch64-apple-darwin/cargo-deny"
		}},
		{os: "darwin", arch: "x86_64", source: schema.#GitHub & {
			repo:  "EmbarkStudios/cargo-deny"
			asset: "cargo-deny-{version}-x86_64-apple-darwin.tar.gz"
			path:  "cargo-deny-{version}-x86_64-apple-darwin/cargo-deny"
		}},
		{os: "linux", arch: "x86_64", source: schema.#GitHub & {
			repo:  "EmbarkStudios/cargo-deny"
			asset: "cargo-deny-{version}-x86_64-unknown-linux-musl.tar.gz"
			path:  "cargo-deny-{version}-x86_64-unknown-linux-musl/cargo-deny"
		}},
		{os: "linux", arch: "arm64", source: schema.#GitHub & {
			repo:  "EmbarkStudios/cargo-deny"
			asset: "cargo-deny-{version}-aarch64-unknown-linux-musl.tar.gz"
			path:  "cargo-deny-{version}-aarch64-unknown-linux-musl/cargo-deny"
		}},
	]
}

// #CargoLlvmCov provides code coverage via LLVM from GitHub releases.
#CargoLlvmCov: schema.#Tool & {
	version!: string
	overrides: [
		{os: "darwin", arch: "arm64", source: schema.#GitHub & {
			repo:      "taiki-e/cargo-llvm-cov"
			tagPrefix: "v"
			asset:     "cargo-llvm-cov-aarch64-apple-darwin.tar.gz"
			path:      "cargo-llvm-cov"
		}},
		{os: "darwin", arch: "x86_64", source: schema.#GitHub & {
			repo:      "taiki-e/cargo-llvm-cov"
			tagPrefix: "v"
			asset:     "cargo-llvm-cov-x86_64-apple-darwin.tar.gz"
			path:      "cargo-llvm-cov"
		}},
		{os: "linux", arch: "x86_64", source: schema.#GitHub & {
			repo:      "taiki-e/cargo-llvm-cov"
			tagPrefix: "v"
			asset:     "cargo-llvm-cov-x86_64-unknown-linux-gnu.tar.gz"
			path:      "cargo-llvm-cov"
		}},
		{os: "linux", arch: "arm64", source: schema.#GitHub & {
			repo:      "taiki-e/cargo-llvm-cov"
			tagPrefix: "v"
			asset:     "cargo-llvm-cov-aarch64-unknown-linux-gnu.tar.gz"
			path:      "cargo-llvm-cov"
		}},
	]
}

// #CargoCyclonedx generates SBOM files via Nix (no GitHub releases available).
#CargoCyclonedx: schema.#Tool & {
	version!: string
	source: schema.#Nix & {
		flake:   "nixpkgs"
		package: "cargo-cyclonedx"
	}
}

// #CargoZigbuild enables cross-compilation using Zig from GitHub releases.
// Note: macOS uses a universal binary (apple-darwin), not arch-specific binaries.
#CargoZigbuild: schema.#Tool & {
	version!: string
	overrides: [
		{os: "darwin", arch: "arm64", source: schema.#GitHub & {
			repo:      "rust-cross/cargo-zigbuild"
			tagPrefix: "v"
			asset:     "cargo-zigbuild-v{version}.apple-darwin.tar.gz"
			path:      "cargo-zigbuild"
		}},
		{os: "darwin", arch: "x86_64", source: schema.#GitHub & {
			repo:      "rust-cross/cargo-zigbuild"
			tagPrefix: "v"
			asset:     "cargo-zigbuild-v{version}.apple-darwin.tar.gz"
			path:      "cargo-zigbuild"
		}},
		{os: "linux", arch: "x86_64", source: schema.#GitHub & {
			repo:      "rust-cross/cargo-zigbuild"
			tagPrefix: "v"
			asset:     "cargo-zigbuild-v{version}.x86_64-unknown-linux-musl.tar.gz"
			path:      "cargo-zigbuild"
		}},
		{os: "linux", arch: "arm64", source: schema.#GitHub & {
			repo:      "rust-cross/cargo-zigbuild"
			tagPrefix: "v"
			asset:     "cargo-zigbuild-v{version}.aarch64-unknown-linux-musl.tar.gz"
			path:      "cargo-zigbuild"
		}},
	]
}

// #SccacheTool provides compilation caching from GitHub releases.
#SccacheTool: schema.#Tool & {
	version!: string
	overrides: [
		{os: "darwin", arch: "arm64", source: schema.#GitHub & {
			repo:      "mozilla/sccache"
			tagPrefix: "v"
			asset:     "sccache-v{version}-aarch64-apple-darwin.tar.gz"
			path:      "sccache-v{version}-aarch64-apple-darwin/sccache"
		}},
		{os: "darwin", arch: "x86_64", source: schema.#GitHub & {
			repo:      "mozilla/sccache"
			tagPrefix: "v"
			asset:     "sccache-v{version}-x86_64-apple-darwin.tar.gz"
			path:      "sccache-v{version}-x86_64-apple-darwin/sccache"
		}},
		{os: "linux", arch: "x86_64", source: schema.#GitHub & {
			repo:      "mozilla/sccache"
			tagPrefix: "v"
			asset:     "sccache-v{version}-x86_64-unknown-linux-musl.tar.gz"
			path:      "sccache-v{version}-x86_64-unknown-linux-musl/sccache"
		}},
		{os: "linux", arch: "arm64", source: schema.#GitHub & {
			repo:      "mozilla/sccache"
			tagPrefix: "v"
			asset:     "sccache-v{version}-aarch64-unknown-linux-musl.tar.gz"
			path:      "sccache-v{version}-aarch64-unknown-linux-musl/sccache"
		}},
	]
}

// #Zig provides the Zig toolchain via Nix (required by cargo-zigbuild).
// Note: Zig distributes prebuilt binaries via ziglang.org, not GitHub Releases.
// GitHub only hosts bootstrap tarballs, so we use Nix for reliable distribution.
#Zig: schema.#Tool & {
	version!: string
	source: schema.#Nix & {
		flake:   "nixpkgs"
		package: "zig"
	}
}
