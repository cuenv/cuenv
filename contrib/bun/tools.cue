package bun

import "github.com/cuenv/cuenv/schema"

// #Bun provides a pre-configured tool definition for Bun from GitHub Releases.
//
// Bun's release assets use non-standard arch naming (aarch64/x64 instead of
// arm64/x86_64), so this contrib module handles the platform-specific mapping.
//
// Usage:
//
// import xBun "github.com/cuenv/cuenv/contrib/bun"
//
// runtime: schema.#ToolsRuntime & {
//     tools: {
//         bun: xBun.#Bun & {version: "1.3.5"}
//     }
// }
#Bun: schema.#Tool & {
	version!: string
	overrides: [
		{os: "darwin", arch: "arm64", source: schema.#GitHub & {
			repo:  "oven-sh/bun"
			tag:   "bun-v{version}"
			asset: "bun-darwin-aarch64.zip"
			path:  "bun-darwin-aarch64/bun"
		}},
		{os: "darwin", arch: "x86_64", source: schema.#GitHub & {
			repo:  "oven-sh/bun"
			tag:   "bun-v{version}"
			asset: "bun-darwin-x64.zip"
			path:  "bun-darwin-x64/bun"
		}},
		{os: "linux", arch: "x86_64", source: schema.#GitHub & {
			repo:  "oven-sh/bun"
			tag:   "bun-v{version}"
			asset: "bun-linux-x64.zip"
			path:  "bun-linux-x64/bun"
		}},
		{os: "linux", arch: "arm64", source: schema.#GitHub & {
			repo:  "oven-sh/bun"
			tag:   "bun-v{version}"
			asset: "bun-linux-aarch64.zip"
			path:  "bun-linux-aarch64/bun"
		}},
	]
}
