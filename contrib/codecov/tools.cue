package codecov

import "github.com/cuenv/cuenv/schema"

// #CodecovCLI provides the Codecov CLI tool from GitHub Releases.
//
// The Codecov CLI is used for uploading coverage reports to Codecov.
// Binary assets are named differently per platform (codecovcli_linux,
// codecovcli_macos, etc.).
//
// Usage:
//
// import xCodecov "github.com/cuenv/cuenv/contrib/codecov"
//
// runtime: schema.#ToolsRuntime & {
//     tools: {
//         codecov: xCodecov.#CodecovCLI & {version: "10.4.0"}
//     }
// }
#CodecovCLI: schema.#Tool & {
	version!: string
	overrides: [
		{os: "darwin", arch: "arm64", source: schema.#GitHub & {
			repo:  "codecov/codecov-cli"
			tag:   "v{version}"
			asset: "codecovcli_macos"
		}},
		{os: "darwin", arch: "x86_64", source: schema.#GitHub & {
			repo:  "codecov/codecov-cli"
			tag:   "v{version}"
			asset: "codecovcli_macos"
		}},
		{os: "linux", arch: "x86_64", source: schema.#GitHub & {
			repo:  "codecov/codecov-cli"
			tag:   "v{version}"
			asset: "codecovcli_linux"
		}},
		{os: "linux", arch: "arm64", source: schema.#GitHub & {
			repo:  "codecov/codecov-cli"
			tag:   "v{version}"
			asset: "codecovcli_linux_arm64"
		}},
	]
}
