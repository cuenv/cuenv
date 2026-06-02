//! Build script for compiling the Go CUE bridge

use std::collections::hash_map::DefaultHasher;
use std::env;
use std::error::Error;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

type BuildResult<T> = Result<T, Box<dyn Error>>;

fn main() -> BuildResult<()> {
    // Skip entire build script on docs.rs - no Go toolchain available
    if env::var("DOCS_RS").is_ok() {
        println!("cargo:warning=Skipping Go FFI build for docs.rs");
        return Ok(());
    }

    let build = BridgeBuild::from_env()?;
    emit_rerun_directives(&build.bridge_dir)?;
    build.log_debug_context()?;
    report_go_version();

    // Try to use prebuilt artifacts first (produced by Nix/flake builds), but
    // force a local rebuild when the tracked Go sources changed since the last
    // successful build in this OUT_DIR. This avoids stale prebuilt archives when
    // iterating on bridge.go.
    let workspace_root = workspace_root(&build.bridge_dir)?;
    let source_fingerprint = bridge_source_fingerprint(&build.bridge_dir)?;
    let fingerprint_path = build.out_dir.join("libcue_bridge.fingerprint");
    let sources_changed = bridge_sources_changed(&fingerprint_path, &source_fingerprint);

    if sources_changed {
        println!("cargo:warning=Go bridge sources changed; rebuilding bridge from source");
    }

    let prebuilt_request = PrebuiltBridgeRequest {
        lib_filename: build.outputs.lib_filename,
        bridge_dir: &build.bridge_dir,
        workspace_root: &workspace_root,
        output_path: &build.outputs.output_path,
        header_path: &build.outputs.header_path,
    };

    if sources_changed || !try_use_prebuilt(&prebuilt_request)? {
        build_go_bridge(
            &build.bridge_dir,
            &build.outputs.output_path,
            &build.target_triple,
        )?;

        if !build.outputs.output_path.exists() {
            return Err(build_error(format!(
                "Go bridge library was not created at expected path: {}",
                build.outputs.output_path.display()
            )));
        }
        println!(
            "Successfully created library at: {}",
            build.outputs.output_path.display()
        );
    }

    write_bridge_fingerprint(&fingerprint_path, &source_fingerprint)?;
    configure_rustc_linking(&build.target_triple, &build.out_dir);
    Ok(())
}

fn build_error(message: impl Into<String>) -> Box<dyn Error> {
    io::Error::other(message.into()).into()
}

struct BridgeBuild {
    bridge_dir: PathBuf,
    out_dir: PathBuf,
    target_triple: String,
    outputs: BridgeOutputs,
}

impl BridgeBuild {
    fn from_env() -> BuildResult<Self> {
        let bridge_dir =
            PathBuf::from(env::var("CARGO_MANIFEST_DIR").map_err(|_| {
                io::Error::new(io::ErrorKind::NotFound, "CARGO_MANIFEST_DIR not set")
            })?);
        let out_dir =
            PathBuf::from(env::var("OUT_DIR").map_err(|_| {
                io::Error::new(io::ErrorKind::NotFound, "OUT_DIR not set by cargo")
            })?);
        let target_triple = env::var("TARGET")
            .or_else(|_| env::var("HOST"))
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "Neither TARGET nor HOST set by cargo",
                )
            })?;
        let outputs = BridgeOutputs::new(&out_dir, &target_triple);

        Ok(Self {
            bridge_dir,
            out_dir,
            target_triple,
            outputs,
        })
    }

    fn log_debug_context(&self) -> BuildResult<()> {
        println!("=== CUENGINE BUILD SCRIPT DEBUG ===");
        println!("Building for target: {}", self.target_triple);
        println!("Is Windows: {}", self.target_triple.contains("windows"));
        println!("Expected library: {}", self.outputs.output_path.display());
        println!("Bridge directory: {}", self.bridge_dir.display());
        println!("Out directory: {}", self.out_dir.display());
        println!(
            "Bridge GO file exists: {}",
            self.bridge_dir.join("bridge.go").exists()
        );
        println!(
            "Go bridge source files: {}",
            go_source_paths(&self.bridge_dir)?.len()
        );
        println!(
            "Go bridge build files: {}",
            go_build_source_paths(&self.bridge_dir)?.len()
        );
        Ok(())
    }
}

struct BridgeOutputs {
    lib_filename: &'static str,
    output_path: PathBuf,
    header_path: PathBuf,
}

impl BridgeOutputs {
    fn new(out_dir: &Path, target_triple: &str) -> Self {
        let lib_filename = if target_triple.contains("windows") {
            "libcue_bridge.lib"
        } else {
            "libcue_bridge.a"
        };

        Self {
            lib_filename,
            output_path: out_dir.join(lib_filename),
            header_path: out_dir.join("libcue_bridge.h"),
        }
    }
}

fn emit_rerun_directives(manifest_dir: &Path) -> BuildResult<()> {
    // Track all Go source files and module files for rebuild detection.
    for path in tracked_build_inputs(manifest_dir)? {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    println!("cargo:rerun-if-env-changed=CUE_BRIDGE_PATH");
    Ok(())
}

fn report_go_version() {
    match Command::new("go").arg("version").output() {
        Ok(output) if output.status.success() => {
            println!(
                "Go version: {}",
                String::from_utf8_lossy(&output.stdout).trim()
            );
        }
        Ok(_) => println!("Go command failed"),
        Err(e) => println!("Go not available: {e}"),
    }
}

fn workspace_root(bridge_dir: &Path) -> BuildResult<PathBuf> {
    env::var("CARGO_WORKSPACE_DIR").map_or_else(
        |_| {
            bridge_dir
                .parent()
                .and_then(Path::parent)
                .map(Path::to_path_buf)
                .ok_or_else(|| {
                    build_error(format!(
                        "Failed to derive workspace root from crate manifest directory: {}",
                        bridge_dir.display()
                    ))
                })
        },
        |path| Ok(PathBuf::from(path)),
    )
}

fn bridge_source_fingerprint(bridge_dir: &Path) -> BuildResult<String> {
    let mut hasher = DefaultHasher::new();

    for path in tracked_go_paths(bridge_dir)? {
        path.hash(&mut hasher);
        if let Ok(bytes) = fs::read(&path) {
            bytes.hash(&mut hasher);
        }
    }

    Ok(format!("{:016x}", hasher.finish()))
}

fn bridge_sources_changed(fingerprint_path: &Path, source_fingerprint: &str) -> bool {
    fs::read_to_string(fingerprint_path).is_ok_and(|existing| existing != source_fingerprint)
}

fn write_bridge_fingerprint(fingerprint_path: &Path, source_fingerprint: &str) -> io::Result<()> {
    fs::write(fingerprint_path, source_fingerprint)
}

fn tracked_build_inputs(bridge_dir: &Path) -> BuildResult<Vec<PathBuf>> {
    let mut paths = vec![
        bridge_dir.join("build.rs"),
        bridge_dir.join("bridge.h"),
        bridge_dir.join("go.mod"),
        bridge_dir.join("go.sum"),
    ];
    paths.extend(go_source_paths(bridge_dir)?);
    paths.sort();
    Ok(paths)
}

fn tracked_go_paths(bridge_dir: &Path) -> BuildResult<Vec<PathBuf>> {
    let mut paths = vec![bridge_dir.join("go.mod"), bridge_dir.join("go.sum")];
    paths.extend(go_source_paths(bridge_dir)?);
    paths.sort();
    Ok(paths)
}

fn go_source_paths(bridge_dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut paths = fs::read_dir(bridge_dir)
        .map_err(|error| {
            io::Error::new(
                error.kind(),
                format!(
                    "Failed to read bridge source directory {}: {error}",
                    bridge_dir.display()
                ),
            )
        })?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("go"))
        .collect::<Vec<_>>();
    paths.sort();
    Ok(paths)
}

fn go_build_source_paths(bridge_dir: &Path) -> io::Result<Vec<PathBuf>> {
    Ok(go_source_paths(bridge_dir)?
        .into_iter()
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| !name.ends_with("_test.go"))
        })
        .collect())
}

struct PrebuiltBridgeRequest<'a> {
    lib_filename: &'a str,
    bridge_dir: &'a Path,
    workspace_root: &'a Path,
    output_path: &'a Path,
    header_path: &'a Path,
}

fn try_use_prebuilt(request: &PrebuiltBridgeRequest<'_>) -> BuildResult<bool> {
    let newest_source_time = newest_tracked_source_time(request.bridge_dir)?;

    for (lib_path, header_path_candidate) in prebuilt_locations(request) {
        if !prebuilt_bridge_exists(&lib_path, &header_path_candidate) {
            continue;
        }

        if prebuilt_is_stale(&lib_path, newest_source_time) {
            println!(
                "cargo:warning=Prebuilt {} is older than source files, will rebuild",
                lib_path.display()
            );
            continue;
        }

        copy_prebuilt_bridge(&lib_path, &header_path_candidate, request)?;
        println!(
            "Using pre-built Go bridge ({}) from: {}",
            prebuilt_build_type(&lib_path),
            lib_path.display()
        );
        return Ok(true);
    }

    Ok(false)
}

fn newest_tracked_source_time(bridge_dir: &Path) -> BuildResult<Option<SystemTime>> {
    Ok(tracked_go_paths(bridge_dir)?
        .iter()
        .filter_map(|path| path.metadata().ok())
        .filter_map(|metadata| metadata.modified().ok())
        .max())
}

fn prebuilt_locations(request: &PrebuiltBridgeRequest<'_>) -> [(PathBuf, PathBuf); 6] {
    let bridge_path = env::var("CUE_BRIDGE_PATH").unwrap_or_default();
    let bridge_override = PathBuf::from(bridge_path);
    let debug_override = bridge_override.join("debug");
    let release_override = bridge_override.join("release");

    [
        // Nix flake puts prebuilt artifacts in workspace target/
        (
            request
                .workspace_root
                .join("target/debug")
                .join(request.lib_filename),
            request.workspace_root.join("target/debug/libcue_bridge.h"),
        ),
        (
            request
                .workspace_root
                .join("target/release")
                .join(request.lib_filename),
            request
                .workspace_root
                .join("target/release/libcue_bridge.h"),
        ),
        // Local development builds
        (
            request
                .bridge_dir
                .join("target/debug")
                .join(request.lib_filename),
            request.bridge_dir.join("target/debug/libcue_bridge.h"),
        ),
        (
            request
                .bridge_dir
                .join("target/release")
                .join(request.lib_filename),
            request.bridge_dir.join("target/release/libcue_bridge.h"),
        ),
        // Environment variable override (useful for CI/Nix)
        (
            debug_override.clone().join(request.lib_filename),
            debug_override.join("libcue_bridge.h"),
        ),
        (
            release_override.clone().join(request.lib_filename),
            release_override.join("libcue_bridge.h"),
        ),
    ]
}

fn prebuilt_bridge_exists(lib_path: &Path, header_path: &Path) -> bool {
    lib_path.is_file() && header_path.is_file()
}

fn prebuilt_is_stale(lib_path: &Path, newest_source_time: Option<SystemTime>) -> bool {
    let Some(source_time) = newest_source_time else {
        return false;
    };

    let Ok(lib_meta) = lib_path.metadata() else {
        return false;
    };

    lib_meta
        .modified()
        .is_ok_and(|lib_time| lib_time < source_time)
}

fn copy_prebuilt_bridge(
    lib_path: &Path,
    header_path_candidate: &Path,
    request: &PrebuiltBridgeRequest<'_>,
) -> io::Result<()> {
    // Remove destination files if they exist (might be read-only).
    let _ = std::fs::remove_file(request.output_path);
    let _ = std::fs::remove_file(request.header_path);

    std::fs::copy(lib_path, request.output_path).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!(
                "Failed to copy pre-built bridge from {}: {error}",
                lib_path.display()
            ),
        )
    })?;
    std::fs::copy(header_path_candidate, request.header_path).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!(
                "Failed to copy pre-built header from {}: {error}",
                header_path_candidate.display()
            ),
        )
    })?;
    Ok(())
}

fn prebuilt_build_type(lib_path: &Path) -> &'static str {
    if lib_path.to_string_lossy().contains("release") {
        "release"
    } else {
        "debug"
    }
}

fn build_go_bridge(bridge_dir: &Path, output_path: &Path, target_triple: &str) -> BuildResult<()> {
    // Build the Go static archive with CGO (fallback for non-Nix builds)
    println!("Building Go bridge from source");
    let mut cmd = Command::new("go");
    cmd.current_dir(bridge_dir).arg("build");
    cmd.env("CGO_ENABLED", "1");

    // Keep Go's target in sync with Rust target when cross-compiling
    if let Some(go_os) = go_os_from_target(target_triple) {
        cmd.env("GOOS", go_os);
    }
    if let Some(go_arch) = go_arch_from_target(target_triple) {
        cmd.env("GOARCH", go_arch);
    }

    // Cross-compile to Linux using Zig - the one true way
    if target_triple.contains("linux") {
        let host_triple = env::var("HOST").unwrap_or_else(|_| target_triple.to_string());

        // Only configure Zig if we're actually cross-compiling
        if host_triple != target_triple {
            // Check if zig is available
            if Command::new("zig").arg("version").output().is_ok() {
                // Map Rust target to Zig target format
                let zig_arch = if target_triple.starts_with("x86_64") {
                    "x86_64"
                } else if target_triple.starts_with("aarch64") {
                    "aarch64"
                } else {
                    return Err(build_error(format!(
                        "Unsupported cross-compilation architecture: {target_triple}"
                    )));
                };

                let zig_target = format!("{zig_arch}-linux-gnu");

                println!(
                    "cargo:warning=Configuring Zig cross-compilation toolchain for {zig_target}"
                );
                cmd.env("CC", format!("zig cc -target {zig_target}"));
                cmd.env("CXX", format!("zig c++ -target {zig_target}"));
                cmd.env("AR", "zig ar");
            } else {
                return Err(build_error(format!(
                    "Cross-compiling from {host_triple} to {target_triple} requires Zig.\n\
                     Install Zig (https://ziglang.org/download/) or set CUE_BRIDGE_PATH."
                )));
            }
        }
    }

    // Set macOS deployment target to match Rust's default (11.0)
    // This prevents version mismatch linker errors when building on newer macOS
    #[cfg(target_os = "macos")]
    {
        cmd.env("MACOSX_DEPLOYMENT_TARGET", "11.0");
        cmd.env("CGO_CFLAGS", "-mmacosx-version-min=11.0");
        cmd.env("CGO_LDFLAGS", "-mmacosx-version-min=11.0");
    }

    // Use vendor directory if it exists (for Nix builds)
    if bridge_dir.join("vendor").exists() {
        cmd.arg("-mod=vendor");
        println!("Using vendor directory");
    }

    cmd.arg("-buildmode=c-archive").arg("-o").arg(output_path);
    cmd.args(go_build_source_paths(bridge_dir)?);

    println!("Running Go command: {cmd:?}");

    let output = cmd.output().map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("Failed to execute Go command. Make sure Go is installed: {error}"),
        )
    })?;

    if !output.status.success() {
        println!("Go build failed!");
        println!("stdout: {}", String::from_utf8_lossy(&output.stdout));
        println!("stderr: {}", String::from_utf8_lossy(&output.stderr));
        return Err(build_error("Failed to build libcue bridge"));
    }

    println!("Go build completed successfully");
    Ok(())
}

fn go_os_from_target(target_triple: &str) -> Option<&'static str> {
    if target_triple.contains("darwin") {
        Some("darwin")
    } else if target_triple.contains("linux") {
        Some("linux")
    } else if target_triple.contains("windows") {
        Some("windows")
    } else {
        None
    }
}

fn go_arch_from_target(target_triple: &str) -> Option<&'static str> {
    if target_triple.starts_with("aarch64") {
        Some("arm64")
    } else if target_triple.starts_with("x86_64") {
        Some("amd64")
    } else {
        None
    }
}

fn configure_rustc_linking(target_triple: &str, out_dir: &Path) {
    // Tell Rust where to find the library
    println!("cargo:rustc-link-search=native={}", out_dir.display());
    println!("cargo:rustc-link-lib=static=cue_bridge");

    // Link system libraries that Go runtime needs
    if target_triple.contains("windows") {
        // Windows-specific libraries
        println!("cargo:rustc-link-lib=ws2_32");
        println!("cargo:rustc-link-lib=userenv");
        println!("cargo:rustc-link-lib=ntdll");
        println!("cargo:rustc-link-lib=winmm");
        // MSVC CRT shims: required for fprintf and related stdio symbols
        // Fixes: link.exe LNK2019 unresolved external symbol fprintf
        println!("cargo:rustc-link-lib=legacy_stdio_definitions");
    } else {
        // Unix-like systems
        println!("cargo:rustc-link-lib=pthread");
        println!("cargo:rustc-link-lib=m");
        println!("cargo:rustc-link-lib=dl");

        if target_triple.contains("apple") || target_triple.contains("darwin") {
            // macOS requires Security framework for certificate validation
            println!("cargo:rustc-link-lib=framework=Security");
            println!("cargo:rustc-link-lib=framework=CoreFoundation");
            println!("cargo:rustc-link-lib=framework=SystemConfiguration");
        }
    }
}
