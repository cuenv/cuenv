package contributors

import "github.com/cuenv/cuenv/schema"

// #Cuenv installs or builds cuenv in CI environments.
//
// Always active - cuenv is needed to run tasks.
// The installation method depends on config.ci.cuenv.source:
// - release (default): Download pre-built binary from GitHub Releases
// - git: Build from git checkout (requires Nix)
// - nix: Install via Nix flake
// - homebrew: Install via Homebrew tap (no Nix required)
//
// Contributes to Setup phase with priority 10.
//
// Usage:
//
//	import "github.com/cuenv/cuenv/contrib/contributors"
//
//	ci: contributors: [contributors.#Cuenv]
#Cuenv: schema.#Contributor & {
	id: "cuenv"
	when: always: true
	tasks: [{
		id:       "setup-cuenv"
		phase:    "setup"
		label:    "Setup cuenv"
		priority: 10
		shell:    true
		env: GITHUB_TOKEN: "${{ secrets.GITHUB_TOKEN }}"
		// Default: release mode (download pre-built binary)
		// The actual command is templated at runtime based on config.ci.cuenv.source
		// TODO: Add SHA256 checksum verification for downloaded binary
		command: """
			curl -sSL -o /usr/local/bin/cuenv https://github.com/cuenv/cuenv/releases/latest/download/cuenv-linux-x64 && \\
			chmod +x /usr/local/bin/cuenv && \\
			/usr/local/bin/cuenv sync -A
			"""
	}]
}

// #CuenvRelease installs cuenv from GitHub Releases (default mode)
// No Nix dependency required.
// TODO: Add SHA256 checksum verification for downloaded binary
#CuenvRelease: schema.#Contributor & {
	id: "cuenv"
	when: cuenvSource: ["release"]
	tasks: [{
		id:       "setup-cuenv"
		phase:    "setup"
		label:    "Setup cuenv (release)"
		priority: 10
		shell:    true
		env: GITHUB_TOKEN: "${{ secrets.GITHUB_TOKEN }}"
		command: """
			curl -sSL -o /usr/local/bin/cuenv https://github.com/cuenv/cuenv/releases/latest/download/cuenv-linux-x64 && \\
			chmod +x /usr/local/bin/cuenv && \\
			/usr/local/bin/cuenv sync -A
			"""
	}]
}

// #CuenvGit builds cuenv from git checkout using Nix
// Requires install-nix to have run first.
#CuenvGit: schema.#Contributor & {
	id: "cuenv"
	when: cuenvSource: ["git"]
	tasks: [{
		id:        "setup-cuenv"
		phase:     "setup"
		label:     "Build cuenv"
		priority:  10
		shell:     true
		dependsOn: ["install-nix"]
		env: GITHUB_TOKEN: "${{ secrets.GITHUB_TOKEN }}"
		command: """
			. /nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh && \\
			nix develop -c cargo build --release -p cuenv && \\
			{ echo "$(pwd)/target/release" >> "$GITHUB_PATH" 2>/dev/null || \\
			  echo "$(pwd)/target/release" >> "$BUILDKITE_ENV_FILE" 2>/dev/null || true; } && \\
			./target/release/cuenv sync -A
			"""
	}]
}

// #CuenvNative builds cuenv using native Rust/Go toolchains (no Nix)
// build.rs automatically compiles the Go CUE bridge via `go build`
#CuenvNative: schema.#Contributor & {
	id: "cuenv"
	when: cuenvSource: ["native"]
	tasks: [
		{
			id:       "setup-rust"
			phase:    "setup"
			label:    "Setup Rust"
			priority: 6
			provider: github: uses: "dtolnay/rust-toolchain@stable"
		},
		{
			id:       "setup-go"
			phase:    "setup"
			label:    "Setup Go"
			priority: 6
			provider: github: {
				uses: "actions/setup-go@v5"
				with: "go-version": "1.24"
			}
		},
		{
			id:        "setup-cuenv"
			phase:     "setup"
			label:     "Build cuenv"
			priority:  10
			shell:     true
			dependsOn: ["setup-rust", "setup-go"]
			env: GITHUB_TOKEN: "${{ secrets.GITHUB_TOKEN }}"
			command: """
				cargo build --release -p cuenv && \\
				{ echo "$(pwd)/target/release" >> "$GITHUB_PATH" 2>/dev/null || \\
				  echo "$(pwd)/target/release" >> "$BUILDKITE_ENV_FILE" 2>/dev/null || true; } && \\
				./target/release/cuenv sync -A
				"""
		},
	]
}

// #CuenvFromArtifact sets up cuenv from a previously built artifact
// Used by CI jobs that depend on the build.cuenv job
#CuenvFromArtifact: schema.#Contributor & {
	id: "cuenv"
	when: cuenvSource: ["artifact"]
	tasks: [{
		id:       "setup-cuenv"
		phase:    "setup"
		label:    "Setup cuenv (from artifact)"
		priority: 10
		shell:    true
		command: """
			chmod +x target/release/cuenv && \\
			{ echo "$(pwd)/target/release" >> "$GITHUB_PATH" 2>/dev/null || \\
			  echo "$(pwd)/target/release" >> "$BUILDKITE_ENV_FILE" 2>/dev/null || true; } && \\
			./target/release/cuenv sync -A
			"""
	}]
}

// #CuenvNix installs cuenv via Nix flake
// Requires install-nix to have run first.
#CuenvNix: schema.#Contributor & {
	id: "cuenv"
	when: cuenvSource: ["nix"]
	tasks: [{
		id:        "setup-cuenv"
		phase:     "setup"
		label:     "Setup cuenv (nix)"
		priority:  10
		shell:     true
		dependsOn: ["install-nix"]
		env: GITHUB_TOKEN: "${{ secrets.GITHUB_TOKEN }}"
		command: """
			. /nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh && \\
			nix profile install github:cuenv/cuenv#cuenv --accept-flake-config && \\
			cuenv sync -A
			"""
	}]
}

// #CuenvHomebrew installs cuenv via Homebrew tap
// No Nix dependency required.
#CuenvHomebrew: schema.#Contributor & {
	id: "cuenv"
	when: cuenvSource: ["homebrew"]
	tasks: [{
		id:       "setup-cuenv"
		phase:    "setup"
		label:    "Setup cuenv (homebrew)"
		priority: 10
		shell:    true
		env: GITHUB_TOKEN: "${{ secrets.GITHUB_TOKEN }}"
		command: """
			brew install cuenv/cuenv/cuenv && \\
			cuenv sync -A
			"""
	}]
}
