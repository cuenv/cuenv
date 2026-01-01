package stages

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
// Contributes to Setup stage with priority 10.
//
// Usage:
//
//	import stages "github.com/cuenv/cuenv/contrib/stages"
//
//	ci: stageContributors: [stages.#Cuenv]
#Cuenv: schema.#StageContributor & {
	id: "cuenv"
	when: always: true
	tasks: [{
		id:       "setup-cuenv"
		stage:    "setup"
		label:    "Setup cuenv"
		priority: 10
		shell:    true
		env: GITHUB_TOKEN: "${{ secrets.GITHUB_TOKEN }}"
		// Default: release mode (download pre-built binary)
		// The actual command is templated at runtime based on config.ci.cuenv.source
		command: """
			curl -sSL -o /usr/local/bin/cuenv https://github.com/cuenv/cuenv/releases/latest/download/cuenv-linux-x64 && \\
			chmod +x /usr/local/bin/cuenv && \\
			/usr/local/bin/cuenv sync -A
			"""
	}]
}

// #CuenvRelease installs cuenv from GitHub Releases (default mode)
// No Nix dependency required.
#CuenvRelease: schema.#StageContributor & {
	id: "cuenv"
	when: cuenvSource: ["release"]
	tasks: [{
		id:       "setup-cuenv"
		stage:    "setup"
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
#CuenvGit: schema.#StageContributor & {
	id: "cuenv"
	when: cuenvSource: ["git"]
	tasks: [{
		id:        "setup-cuenv"
		stage:     "setup"
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

// #CuenvNix installs cuenv via Nix flake
// Requires install-nix to have run first.
#CuenvNix: schema.#StageContributor & {
	id: "cuenv"
	when: cuenvSource: ["nix"]
	tasks: [{
		id:        "setup-cuenv"
		stage:     "setup"
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
#CuenvHomebrew: schema.#StageContributor & {
	id: "cuenv"
	when: cuenvSource: ["homebrew"]
	tasks: [{
		id:       "setup-cuenv"
		stage:    "setup"
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
