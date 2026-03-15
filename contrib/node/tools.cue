package node

import "github.com/cuenv/cuenv/schema"

// #Node provides official Node.js distributions from nodejs.org.
//
// cuenv installs the upstream archive as-is so the full runtime prefix is
// available: `node`, `npm`, `npx`, headers, and bundled libraries.
//
// Note: Node.js 25 no longer bundles `corepack` in the official archives. This
// module follows upstream behavior rather than reintroducing it.
//
// Usage:
//
//	import xNode "github.com/cuenv/cuenv/contrib/node"
//
//	runtime: schema.#ToolsRuntime & {
//	    tools: node: xNode.#Node & {version: "24.14.0"}
//	}
#Node: schema.#Tool & {
	version!: string
	overrides: [
		{os: "darwin", arch: "arm64", source: schema.#URL & {
			url: "https://nodejs.org/dist/v{version}/node-v{version}-darwin-arm64.tar.gz"
		}},
		{os: "darwin", arch: "x86_64", source: schema.#URL & {
			url: "https://nodejs.org/dist/v{version}/node-v{version}-darwin-x64.tar.gz"
		}},
		{os: "linux", arch: "arm64", source: schema.#URL & {
			url: "https://nodejs.org/dist/v{version}/node-v{version}-linux-arm64.tar.gz"
		}},
		{os: "linux", arch: "x86_64", source: schema.#URL & {
			url: "https://nodejs.org/dist/v{version}/node-v{version}-linux-x64.tar.gz"
		}},
	]
}
