package rust

import "github.com/cuenv/cuenv/schema"

// #Sccache provides sccache setup for Rust compilation caching.
//
// Configures cargo to use sccache which should be available via runtime.tools.
// On GitHub Actions, uses Mozilla-Actions/sccache-action for optimized caching.
//
// Usage:
//
// import rustcontrib "github.com/cuenv/cuenv/contrib/rust"
//
// ci: contributors: sccache: rustcontrib.#Sccache
//
// Requires sccache in runtime.tools:
//
//	runtime: schema.#ToolsRuntime & {
//	    tools: {
//	        sccache: #SccacheTool & {version: "0.12.0"}
//	    }
//	}
#Sccache: schema.#Contributor & {
	setup: [{
		name: "Setup sccache"
		script: """
			# Configure cargo to use sccache (provided via cuenv tools)
			export RUSTC_WRAPPER=sccache
			"""
		env: RUSTC_WRAPPER: "sccache"
		// GitHub-specific: use action instead of shell for better caching
		provider: github: uses: "mozilla-actions/sccache-action@v0.0.9"
	}]
}
