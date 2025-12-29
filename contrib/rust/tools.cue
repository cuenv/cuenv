package rust

import "github.com/cuenv/cuenv/schema"

// #Rust provides the Rust toolchain via Nix (cargo, rustc, clippy, rustfmt, rustdoc).
//
// Usage:
//
//	runtime: schema.#ToolsRuntime & {
//	    flakes: nixpkgs: "github:NixOS/nixpkgs/nixos-unstable"
//	    tools: rust: xRust.#Rust & {version: "1.83.0"}
//	}
#Rust: schema.#Tool & {
	version!: string
	source: schema.#Nix & {
		flake:   "nixpkgs"
		package: "cargo"
	}
}

// #RustAnalyzer provides the rust-analyzer LSP server from GitHub releases.
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
			asset: "cargo-nextest-{version}-aarch64-apple-darwin.tar.gz"
			path:  "cargo-nextest"
		}},
		{os: "darwin", arch: "x86_64", source: schema.#GitHub & {
			repo:  "nextest-rs/nextest"
			tag:   "cargo-nextest-{version}"
			asset: "cargo-nextest-{version}-x86_64-apple-darwin.tar.gz"
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
			repo:  "taiki-e/cargo-llvm-cov"
			asset: "cargo-llvm-cov-aarch64-apple-darwin.tar.gz"
			path:  "cargo-llvm-cov"
		}},
		{os: "darwin", arch: "x86_64", source: schema.#GitHub & {
			repo:  "taiki-e/cargo-llvm-cov"
			asset: "cargo-llvm-cov-x86_64-apple-darwin.tar.gz"
			path:  "cargo-llvm-cov"
		}},
		{os: "linux", arch: "x86_64", source: schema.#GitHub & {
			repo:  "taiki-e/cargo-llvm-cov"
			asset: "cargo-llvm-cov-x86_64-unknown-linux-gnu.tar.gz"
			path:  "cargo-llvm-cov"
		}},
		{os: "linux", arch: "arm64", source: schema.#GitHub & {
			repo:  "taiki-e/cargo-llvm-cov"
			asset: "cargo-llvm-cov-aarch64-unknown-linux-gnu.tar.gz"
			path:  "cargo-llvm-cov"
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
#CargoZigbuild: schema.#Tool & {
	version!: string
	overrides: [
		{os: "darwin", arch: "arm64", source: schema.#GitHub & {
			repo:  "rust-cross/cargo-zigbuild"
			asset: "cargo-zigbuild-v{version}.aarch64-apple-darwin.tar.gz"
			path:  "cargo-zigbuild"
		}},
		{os: "darwin", arch: "x86_64", source: schema.#GitHub & {
			repo:  "rust-cross/cargo-zigbuild"
			asset: "cargo-zigbuild-v{version}.x86_64-apple-darwin.tar.gz"
			path:  "cargo-zigbuild"
		}},
		{os: "linux", arch: "x86_64", source: schema.#GitHub & {
			repo:  "rust-cross/cargo-zigbuild"
			asset: "cargo-zigbuild-v{version}.x86_64-unknown-linux-musl.tar.gz"
			path:  "cargo-zigbuild"
		}},
		{os: "linux", arch: "arm64", source: schema.#GitHub & {
			repo:  "rust-cross/cargo-zigbuild"
			asset: "cargo-zigbuild-v{version}.aarch64-unknown-linux-musl.tar.gz"
			path:  "cargo-zigbuild"
		}},
	]
}

// #SccacheTool provides compilation caching from GitHub releases.
#SccacheTool: schema.#Tool & {
	version!: string
	overrides: [
		{os: "darwin", arch: "arm64", source: schema.#GitHub & {
			repo:  "mozilla/sccache"
			asset: "sccache-v{version}-aarch64-apple-darwin.tar.gz"
			path:  "sccache-v{version}-aarch64-apple-darwin/sccache"
		}},
		{os: "darwin", arch: "x86_64", source: schema.#GitHub & {
			repo:  "mozilla/sccache"
			asset: "sccache-v{version}-x86_64-apple-darwin.tar.gz"
			path:  "sccache-v{version}-x86_64-apple-darwin/sccache"
		}},
		{os: "linux", arch: "x86_64", source: schema.#GitHub & {
			repo:  "mozilla/sccache"
			asset: "sccache-v{version}-x86_64-unknown-linux-musl.tar.gz"
			path:  "sccache-v{version}-x86_64-unknown-linux-musl/sccache"
		}},
		{os: "linux", arch: "arm64", source: schema.#GitHub & {
			repo:  "mozilla/sccache"
			asset: "sccache-v{version}-aarch64-unknown-linux-musl.tar.gz"
			path:  "sccache-v{version}-aarch64-unknown-linux-musl/sccache"
		}},
	]
}

// #Zig provides the Zig toolchain from GitHub releases (required by cargo-zigbuild).
#Zig: schema.#Tool & {
	version!: string
	overrides: [
		{os: "darwin", arch: "arm64", source: schema.#GitHub & {
			repo:  "ziglang/zig"
			asset: "zig-macos-aarch64-{version}.tar.xz"
			path:  "zig-macos-aarch64-{version}/zig"
		}},
		{os: "darwin", arch: "x86_64", source: schema.#GitHub & {
			repo:  "ziglang/zig"
			asset: "zig-macos-x86_64-{version}.tar.xz"
			path:  "zig-macos-x86_64-{version}/zig"
		}},
		{os: "linux", arch: "x86_64", source: schema.#GitHub & {
			repo:  "ziglang/zig"
			asset: "zig-linux-x86_64-{version}.tar.xz"
			path:  "zig-linux-x86_64-{version}/zig"
		}},
		{os: "linux", arch: "arm64", source: schema.#GitHub & {
			repo:  "ziglang/zig"
			asset: "zig-linux-aarch64-{version}.tar.xz"
			path:  "zig-linux-aarch64-{version}/zig"
		}},
	]
}
