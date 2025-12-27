package rust

import "github.com/cuenv/cuenv/schema"

// #Sccache provides sccache setup for Rust compilation caching.
//
// Uses shell commands by default to install and configure sccache.
// On GitHub Actions, uses Mozilla-Actions/sccache-action for optimized caching.
//
// Usage:
//
// import rustcontrib "github.com/cuenv/cuenv/contrib/rust"
//
// ci: contributors: sccache: rustcontrib.#Sccache
#Sccache: schema.#Contributor & {
	setup: [{
		name: "Setup sccache"
		script: """
			# Install sccache if not present
			if ! command -v sccache &> /dev/null; then
			    cargo install sccache --locked
			fi
			# Configure cargo to use sccache
			export RUSTC_WRAPPER=sccache
			"""
		env: RUSTC_WRAPPER: "sccache"
		// GitHub-specific: use action instead of shell for better caching
		provider: github: uses: "mozilla-actions/sccache-action@v0.0.9"
	}]
}
