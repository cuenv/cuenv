# Changelog

## [0.2.0](https://github.com/cuenv/cuenv/compare/cuengine-v0.1.0...cuengine-v0.2.0) (2025-09-13)


### Features

* add bridge version diagnostics functionality ([ee255b8](https://github.com/cuenv/cuenv/commit/ee255b8ba9be5ef5e1d0902d055f878158864c12))
* add C header files and Go bridge tests ([c388909](https://github.com/cuenv/cuenv/commit/c388909811c1d0ac6fd9426740cafca6d6332e57))
* add comprehensive CI/CD pipeline with trusted publishing ([a18e86d](https://github.com/cuenv/cuenv/commit/a18e86d4f273adc6469ceae2bb3133fbe36857f1))
* Add validated PackageDir and PackageName newtypes for API hygiene ([9d98d34](https://github.com/cuenv/cuenv/commit/9d98d34a2649f63360a9ce4ab2f24039444fc293))
* implement background hook execution system with approval-based security ([#31](https://github.com/cuenv/cuenv/issues/31)) ([503d0f1](https://github.com/cuenv/cuenv/commit/503d0f1d6eda6010a5cc2d74ddcbc3c0de3eb3be))
* implement background hooks with approval mechanism and BDD testing ([#35](https://github.com/cuenv/cuenv/issues/35)) ([abbdac2](https://github.com/cuenv/cuenv/commit/abbdac20334d525a6ab7d1506a879320fa9fded0))
* implement event-driven CLI with comprehensive test coverage ([baaccef](https://github.com/cuenv/cuenv/commit/baaccef0ad4dc92e0982a0f186bd8bae9194c54d))
* implement Go-Rust FFI bridge for CUE evaluation ([b0eeb10](https://github.com/cuenv/cuenv/commit/b0eeb1077c6a2a210323c37e1664d23fbb7b54cd))


### Bug Fixes

* address Rust Edition 2024 compatibility and CI improvements ([bef2a43](https://github.com/cuenv/cuenv/commit/bef2a43cc03cbccae8b552e096ff229dfc501d9b))
* apply cargo fmt to pass flake checks ([7baccc3](https://github.com/cuenv/cuenv/commit/7baccc375d99e12bccd84190645582270add03c7))
* change cuengine license to MIT only and add license.md ([d56dc31](https://github.com/cuenv/cuenv/commit/d56dc31e9b242a4f272148e85f2089f282a0c622))
* ensure codecov is available / treefmt ([016beb5](https://github.com/cuenv/cuenv/commit/016beb5742c69bbd482e77fcc87a67f018a32106))
* format ([a23bba1](https://github.com/cuenv/cuenv/commit/a23bba1fcc312be747e721425f8eebe865a7c1c5))
* **lint:** resolve clippy warnings and improve code quality ([f530d84](https://github.com/cuenv/cuenv/commit/f530d840c93d465aae6d4e768427ad9c720ffea7))
* **lint:** resolve uninlined_format_args clippy warnings ([019f2e0](https://github.com/cuenv/cuenv/commit/019f2e0956a5fa3946f19bcbe948be556fc82dcb))
* macOS tests and remove header checkss ([0619392](https://github.com/cuenv/cuenv/commit/06193922ca4f221c1e3585b5e47a363f0951c4e3))
* resolve build and CI issues ([b280846](https://github.com/cuenv/cuenv/commit/b280846c561e56146109262763db9f4ff2237a9e))
* resolve CI failures by fixing formatting and clippy warnings ([726174d](https://github.com/cuenv/cuenv/commit/726174dfe78067d879e7ce90f33e33a3ac86183d))
* resolve circular dependency between cuenv-core and cuengine ([#33](https://github.com/cuenv/cuenv/issues/33)) ([d0cafd3](https://github.com/cuenv/cuenv/commit/d0cafd3b429ef0f507d2f5609dfcfd5b0a8f557b))
* resolve clippy collapsible-if warnings ([a669466](https://github.com/cuenv/cuenv/commit/a6694662e56beb2ccd324589496b754838246ad8))
* resolve clippy warnings by using is_ascii_* methods ([f674634](https://github.com/cuenv/cuenv/commit/f6746343ec42ed495e4aea739e3d3d1c00e60602))
* treefmt ([c2c6a8f](https://github.com/cuenv/cuenv/commit/c2c6a8f3d71756acbb060dfefc4581f055a5a48e))
* use explicit versions in Cargo.toml for release-please compatibility ([0053329](https://github.com/cuenv/cuenv/commit/0053329d8e803510113ef395c2cfd7d416f982dd))
* **windows:** link legacy_stdio_definitions to satisfy fprintf from Go c-archive; attempt to unbreak windows-latest CI ([2db8869](https://github.com/cuenv/cuenv/commit/2db8869ce96e35cceaf86e7f898b995d700c72e9))
* **windows:** produce libcue_bridge.lib on MSVC and detect prebuilt .lib; keep legacy_stdio_definitions; retry windows-latest CI ([94ff5c7](https://github.com/cuenv/cuenv/commit/94ff5c7b2644679483c3c0350a5a274f9c5dc8bf))
