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
			repo:  "mikefarah/yq"
			asset: "yq_darwin_arm64"
		}},
		{os: "darwin", arch: "x86_64", source: schema.#GitHub & {
			repo:  "mikefarah/yq"
			asset: "yq_darwin_amd64"
		}},
		{os: "linux", arch: "x86_64", source: schema.#GitHub & {
			repo:  "mikefarah/yq"
			asset: "yq_linux_amd64"
		}},
		{os: "linux", arch: "arm64", source: schema.#GitHub & {
			repo:  "mikefarah/yq"
			asset: "yq_linux_arm64"
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
			repo:  "numtide/treefmt"
			asset: "treefmt_{version}_darwin_arm64.tar.gz"
			path:  "treefmt"
		}},
		{os: "darwin", arch: "x86_64", source: schema.#GitHub & {
			repo:  "numtide/treefmt"
			asset: "treefmt_{version}_darwin_amd64.tar.gz"
			path:  "treefmt"
		}},
		{os: "linux", arch: "x86_64", source: schema.#GitHub & {
			repo:  "numtide/treefmt"
			asset: "treefmt_{version}_linux_amd64.tar.gz"
			path:  "treefmt"
		}},
		{os: "linux", arch: "arm64", source: schema.#GitHub & {
			repo:  "numtide/treefmt"
			asset: "treefmt_{version}_linux_arm64.tar.gz"
			path:  "treefmt"
		}},
	]
}
