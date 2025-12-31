package tools

import "github.com/cuenv/cuenv/schema"

// #Jq provides the jq JSON processor from GitHub releases.
//
// Usage:
//
//	runtime: schema.#ToolsRuntime & {
//	    tools: jq: xTools.#Jq & {version: "1.7.1"}
//	}
#Jq: schema.#Tool & {
	version!: string
	overrides: [
		{os: "darwin", arch: "arm64", source: schema.#GitHub & {
			repo:  "jqlang/jq"
			tag:   "jq-{version}"
			asset: "jq-macos-arm64"
		}},
		{os: "darwin", arch: "x86_64", source: schema.#GitHub & {
			repo:  "jqlang/jq"
			tag:   "jq-{version}"
			asset: "jq-macos-amd64"
		}},
		{os: "linux", arch: "x86_64", source: schema.#GitHub & {
			repo:  "jqlang/jq"
			tag:   "jq-{version}"
			asset: "jq-linux-amd64"
		}},
		{os: "linux", arch: "arm64", source: schema.#GitHub & {
			repo:  "jqlang/jq"
			tag:   "jq-{version}"
			asset: "jq-linux-arm64"
		}},
	]
}

// #Yq provides the yq YAML processor from GitHub releases.
//
// Usage:
//
//	runtime: schema.#ToolsRuntime & {
//	    tools: yq: xTools.#Yq & {version: "4.44.6"}
//	}
#Yq: schema.#Tool & {
	version!: string
	overrides: [
		{os: "darwin", arch: "arm64", source: schema.#GitHub & {
			repo:      "mikefarah/yq"
			tagPrefix: "v"
			asset:     "yq_darwin_arm64"
		}},
		{os: "darwin", arch: "x86_64", source: schema.#GitHub & {
			repo:      "mikefarah/yq"
			tagPrefix: "v"
			asset:     "yq_darwin_amd64"
		}},
		{os: "linux", arch: "x86_64", source: schema.#GitHub & {
			repo:      "mikefarah/yq"
			tagPrefix: "v"
			asset:     "yq_linux_amd64"
		}},
		{os: "linux", arch: "arm64", source: schema.#GitHub & {
			repo:      "mikefarah/yq"
			tagPrefix: "v"
			asset:     "yq_linux_arm64"
		}},
	]
}

// #Treefmt provides the treefmt multi-language formatter from GitHub releases.
//
// Usage:
//
//	runtime: schema.#ToolsRuntime & {
//	    tools: treefmt: xTools.#Treefmt & {version: "2.4.0"}
//	}
#Treefmt: schema.#Tool & {
	version!: string
	overrides: [
		{os: "darwin", arch: "arm64", source: schema.#GitHub & {
			repo:      "numtide/treefmt"
			tagPrefix: "v"
			asset:     "treefmt_{version}_darwin_arm64.tar.gz"
			path:      "treefmt"
		}},
		{os: "darwin", arch: "x86_64", source: schema.#GitHub & {
			repo:      "numtide/treefmt"
			tagPrefix: "v"
			asset:     "treefmt_{version}_darwin_amd64.tar.gz"
			path:      "treefmt"
		}},
		{os: "linux", arch: "x86_64", source: schema.#GitHub & {
			repo:      "numtide/treefmt"
			tagPrefix: "v"
			asset:     "treefmt_{version}_linux_amd64.tar.gz"
			path:      "treefmt"
		}},
		{os: "linux", arch: "arm64", source: schema.#GitHub & {
			repo:      "numtide/treefmt"
			tagPrefix: "v"
			asset:     "treefmt_{version}_linux_arm64.tar.gz"
			path:      "treefmt"
		}},
	]
}

// #Nixfmt provides the nixfmt Nix code formatter from GitHub releases.
//
// Usage:
//
//	runtime: schema.#ToolsRuntime & {
//	    tools: nixfmt: xTools.#Nixfmt & {version: "0.6.0"}
//	}
#Nixfmt: schema.#Tool & {
	version!: string
	overrides: [
		{os: "darwin", arch: "arm64", source: schema.#GitHub & {
			repo:      "NixOS/nixfmt"
			tagPrefix: "v"
			asset:     "nixfmt-aarch64-darwin"
		}},
		{os: "darwin", arch: "x86_64", source: schema.#GitHub & {
			repo:      "NixOS/nixfmt"
			tagPrefix: "v"
			asset:     "nixfmt-x86_64-darwin"
		}},
		{os: "linux", arch: "x86_64", source: schema.#GitHub & {
			repo:      "NixOS/nixfmt"
			tagPrefix: "v"
			asset:     "nixfmt-x86_64-linux"
		}},
		{os: "linux", arch: "arm64", source: schema.#GitHub & {
			repo:      "NixOS/nixfmt"
			tagPrefix: "v"
			asset:     "nixfmt-aarch64-linux"
		}},
	]
}

// #Alejandra provides the Alejandra Nix code formatter from GitHub releases.
//
// Usage:
//
//	runtime: schema.#ToolsRuntime & {
//	    tools: alejandra: xTools.#Alejandra & {version: "3.1.0"}
//	}
#Alejandra: schema.#Tool & {
	version!: string
	overrides: [
		{os: "darwin", arch: "arm64", source: schema.#GitHub & {
			repo:  "kamadorueda/alejandra"
			asset: "alejandra-aarch64-darwin"
		}},
		{os: "darwin", arch: "x86_64", source: schema.#GitHub & {
			repo:  "kamadorueda/alejandra"
			asset: "alejandra-x86_64-darwin"
		}},
		{os: "linux", arch: "x86_64", source: schema.#GitHub & {
			repo:  "kamadorueda/alejandra"
			asset: "alejandra-x86_64-linux"
		}},
		{os: "linux", arch: "arm64", source: schema.#GitHub & {
			repo:  "kamadorueda/alejandra"
			asset: "alejandra-aarch64-linux"
		}},
	]
}

// #Cue provides the CUE language CLI from GitHub releases.
// Includes cue fmt for formatting CUE files.
//
// Usage:
//
//	runtime: schema.#ToolsRuntime & {
//	    tools: cue: xTools.#Cue & {version: "0.15.3"}
//	}
#Cue: schema.#Tool & {
	version!: string
	overrides: [
		{os: "darwin", arch: "arm64", source: schema.#GitHub & {
			repo:      "cue-lang/cue"
			tagPrefix: "v"
			asset:     "cue_v{version}_darwin_arm64.tar.gz"
			path:      "cue"
		}},
		{os: "darwin", arch: "x86_64", source: schema.#GitHub & {
			repo:      "cue-lang/cue"
			tagPrefix: "v"
			asset:     "cue_v{version}_darwin_amd64.tar.gz"
			path:      "cue"
		}},
		{os: "linux", arch: "x86_64", source: schema.#GitHub & {
			repo:      "cue-lang/cue"
			tagPrefix: "v"
			asset:     "cue_v{version}_linux_amd64.tar.gz"
			path:      "cue"
		}},
		{os: "linux", arch: "arm64", source: schema.#GitHub & {
			repo:      "cue-lang/cue"
			tagPrefix: "v"
			asset:     "cue_v{version}_linux_arm64.tar.gz"
			path:      "cue"
		}},
	]
}

// #Go provides the Go toolchain from GitHub releases.
// Includes gofmt for formatting Go files.
//
// Usage:
//
//	runtime: schema.#ToolsRuntime & {
//	    tools: go: xTools.#Go & {version: "1.23.4"}
//	}
#Go: schema.#Tool & {
	version!: string
	overrides: [
		{os: "darwin", arch: "arm64", source: schema.#GitHub & {
			repo:  "golang/go"
			tag:   "go{version}"
			asset: "go{version}.darwin-arm64.tar.gz"
			path:  "go/bin/gofmt"
		}},
		{os: "darwin", arch: "x86_64", source: schema.#GitHub & {
			repo:  "golang/go"
			tag:   "go{version}"
			asset: "go{version}.darwin-amd64.tar.gz"
			path:  "go/bin/gofmt"
		}},
		{os: "linux", arch: "x86_64", source: schema.#GitHub & {
			repo:  "golang/go"
			tag:   "go{version}"
			asset: "go{version}.linux-amd64.tar.gz"
			path:  "go/bin/gofmt"
		}},
		{os: "linux", arch: "arm64", source: schema.#GitHub & {
			repo:  "golang/go"
			tag:   "go{version}"
			asset: "go{version}.linux-arm64.tar.gz"
			path:  "go/bin/gofmt"
		}},
	]
}
