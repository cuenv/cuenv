# Changelog

## [0.2.0](https://github.com/cuenv/cuenv/compare/cuenv-core-v0.1.0...cuenv-core-v0.2.0) (2025-10-09)


### Features

* add comprehensive CI/CD pipeline with trusted publishing ([a18e86d](https://github.com/cuenv/cuenv/commit/a18e86d4f273adc6469ceae2bb3133fbe36857f1))
* add environment variable access policies ([#44](https://github.com/cuenv/cuenv/issues/44)) ([4f737a2](https://github.com/cuenv/cuenv/commit/4f737a217debc649a6c5694dacbccbfa6d543191))
* Add task execution with DAG support using petgraph ([#19](https://github.com/cuenv/cuenv/issues/19)) ([8078685](https://github.com/cuenv/cuenv/commit/807868566cef1c3d9d187e7c0e3b426678cc8236))
* Add validated PackageDir and PackageName newtypes for API hygiene ([9d98d34](https://github.com/cuenv/cuenv/commit/9d98d34a2649f63360a9ce4ab2f24039444fc293))
* implement background hook execution system with approval-based security ([#31](https://github.com/cuenv/cuenv/issues/31)) ([503d0f1](https://github.com/cuenv/cuenv/commit/503d0f1d6eda6010a5cc2d74ddcbc3c0de3eb3be))
* implement background hooks with approval mechanism and BDD testing ([#35](https://github.com/cuenv/cuenv/issues/35)) ([abbdac2](https://github.com/cuenv/cuenv/commit/abbdac20334d525a6ab7d1506a879320fa9fded0))
* implement cuenv-core with error handling and configuration ([c9d8d70](https://github.com/cuenv/cuenv/commit/c9d8d7065e31dbbfcdc5bdc6ede2d143da00ed27))
* implement event-driven CLI with comprehensive test coverage ([baaccef](https://github.com/cuenv/cuenv/commit/baaccef0ad4dc92e0982a0f186bd8bae9194c54d))


### Bug Fixes

* align CUE schemas with Rust types for validation ([a94de75](https://github.com/cuenv/cuenv/commit/a94de75b0ef8dbf9de2acb5706a0b2922f614680))
* apply cargo fmt to pass flake checks ([7baccc3](https://github.com/cuenv/cuenv/commit/7baccc375d99e12bccd84190645582270add03c7))
* fmt and lint corrections ([66fbea1](https://github.com/cuenv/cuenv/commit/66fbea1f2d3caee4a61fdef5b8e6ed67d9436ce1))
* increase test timeout to prevent CI failures ([4472769](https://github.com/cuenv/cuenv/commit/4472769fb8b87a6485e136b20b2459a5a961ffb8))
* resolve CI failures by fixing formatting and clippy warnings ([726174d](https://github.com/cuenv/cuenv/commit/726174dfe78067d879e7ce90f33e33a3ac86183d))
* resolve circular dependency between cuenv-core and cuengine ([#33](https://github.com/cuenv/cuenv/issues/33)) ([d0cafd3](https://github.com/cuenv/cuenv/commit/d0cafd3b429ef0f507d2f5609dfcfd5b0a8f557b))
* resolve clippy warnings across the codebase ([5ade9a9](https://github.com/cuenv/cuenv/commit/5ade9a9370d4a5fb1707c4b6513284f8ea2eec18))
* treefmt ([c2c6a8f](https://github.com/cuenv/cuenv/commit/c2c6a8f3d71756acbb060dfefc4581f055a5a48e))
* use explicit versions in Cargo.toml for release-please compatibility ([0053329](https://github.com/cuenv/cuenv/commit/0053329d8e803510113ef395c2cfd7d416f982dd))
