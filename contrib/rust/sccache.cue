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
// ci: contributors: [rustcontrib.#Sccache]
//
// Requires sccache in runtime.tools:
//
//	runtime: schema.#ToolsRuntime & {
//	    tools: {
//	        sccache: #SccacheTool & {version: "0.12.0"}
//	    }
//	}
#Sccache: schema.#Contributor & {
	id: "sccache"
	when: always: true
	tasks: [
		{
			id:       "setup-sccache"
			phase:    "setup"
			label:    "Setup sccache"
			priority: 5
			script: """
				# Configure cargo to use sccache (provided via cuenv tools)
				export RUSTC_WRAPPER=sccache
				"""
			env: {
				RUSTC_WRAPPER: "sccache"
				SCCACHE_DIR:   "${{ runner.temp }}/sccache"
			}
			provider: github: uses: "mozilla-actions/sccache-action@v0.0.9"
		},
		{
			id:        "export-sccache-env"
			phase:     "setup"
			label:     "Export sccache environment"
			priority:  6
			dependsOn: ["setup-sccache"]
			shell:     true
			command:   "echo \"SCCACHE_DIR=${{ runner.temp }}/sccache\" >> $GITHUB_ENV"
		},
	]
}
