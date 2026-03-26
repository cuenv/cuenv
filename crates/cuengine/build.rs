//! Build script for compiling the Go CUE bridge
//!
//! Build scripts should panic on failure - there's no recovery path for build errors.

// Build scripts are expected to panic/expect on failure - no runtime recovery needed
#![allow(clippy::panic, clippy::expect_used, clippy::too_many_lines)]

use std::collections::hash_map::DefaultHasher;
use std::env;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    // Skip entire build script on docs.rs - no Go toolchain available
    if env::var("DOCS_RS").is_ok() {
        println!("cargo:warning=Skipping Go FFI build for docs.rs");
        return;
    }

    let manifest_dir =
        PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set"));

    // Track all Go source files and module files for rebuild detection.
    for path in [
        "build.rs",
        "bridge.go",
        "bridge_test.go",
        "bridge.h",
        "go.mod",
        "go.sum",
    ] {
        println!(
            "cargo:rerun-if-changed={}",
            manifest_dir.join(path).display()
        );
    }
    println!("cargo:rerun-if-env-changed=CUE_BRIDGE_PATH");

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR not set by cargo"));
    let bridge_dir = manifest_dir;

    // Determine target triple early for platform-specific behavior
    let target_triple = env::var("TARGET")
        .unwrap_or_else(|_| env::var("HOST").expect("Neither TARGET nor HOST set by cargo"));
    let is_windows = target_triple.contains("windows");

    let lib_filename = if is_windows {
        "libcue_bridge.lib"
    } else {
        "libcue_bridge.a"
    };

    let output_path = out_dir.join(lib_filename);
    let header_path = out_dir.join("libcue_bridge.h");

    println!("=== CUENGINE BUILD SCRIPT DEBUG ===");
    println!("Building for target: {target_triple}");
    println!("Is Windows: {is_windows}");
    println!("Expected library: {}", output_path.display());
    println!("Bridge directory: {}", bridge_dir.display());
    println!("Out directory: {}", out_dir.display());
    println!(
        "Bridge GO file exists: {}",
        bridge_dir.join("bridge.go").exists()
    );

    // Check if Go is available
    match std::process::Command::new("go").arg("version").output() {
        Ok(output) if output.status.success() => {
            println!(
                "Go version: {}",
                String::from_utf8_lossy(&output.stdout).trim()
            );
        }
        Ok(_) => println!("Go command failed"),
        Err(e) => println!("Go not available: {e}"),
    }

    // Try to use prebuilt artifacts first (produced by Nix/flake builds), but
    // force a local rebuild when the tracked Go sources changed since the last
    // successful build in this OUT_DIR. This avoids stale prebuilt archives when
    // iterating on bridge.go.
    let workspace_root = env::var("CARGO_WORKSPACE_DIR").map_or_else(
        |_| {
            bridge_dir
                .parent()
                .and_then(|p| p.parent())
                .expect("Failed to derive workspace root from crate manifest directory")
                .to_path_buf()
        },
        PathBuf::from,
    );
    let source_fingerprint = bridge_source_fingerprint(&bridge_dir);
    let fingerprint_path = out_dir.join("libcue_bridge.fingerprint");
    let sources_changed = bridge_sources_changed(&fingerprint_path, &source_fingerprint);

    if sources_changed {
        println!("cargo:warning=Go bridge sources changed; rebuilding bridge from source");
    }

    if sources_changed
        || !try_use_prebuilt(
            lib_filename,
            &bridge_dir,
            &workspace_root,
            &output_path,
            &header_path,
        )
    {
        build_go_bridge(&bridge_dir, &output_path, &target_triple);

        // Verify the library was actually created
        assert!(
            output_path.exists(),
            "Go bridge library was not created at expected path: {}",
            output_path.display()
        );
        println!("Successfully created library at: {}", output_path.display());
    }

    write_bridge_fingerprint(&fingerprint_path, &source_fingerprint);
    configure_rustc_linking(&target_triple, &out_dir);
}

fn bridge_source_fingerprint(bridge_dir: &Path) -> String {
    let mut hasher = DefaultHasher::new();

    for path in tracked_go_paths(bridge_dir) {
        path.hash(&mut hasher);
        if let Ok(bytes) = fs::read(&path) {
            bytes.hash(&mut hasher);
        }
    }

    format!("{:016x}", hasher.finish())
}

fn bridge_sources_changed(fingerprint_path: &Path, source_fingerprint: &str) -> bool {
    fs::read_to_string(fingerprint_path)
        .map(|existing| existing != source_fingerprint)
        .unwrap_or(false)
}

fn write_bridge_fingerprint(fingerprint_path: &Path, source_fingerprint: &str) {
    fs::write(fingerprint_path, source_fingerprint)
        .unwrap_or_else(|e| panic!("Failed to write bridge fingerprint: {e}"));
}

fn tracked_go_paths(bridge_dir: &Path) -> [PathBuf; 4] {
    [
        bridge_dir.join("bridge.go"),
        bridge_dir.join("bridge_test.go"),
        bridge_dir.join("go.mod"),
        bridge_dir.join("go.sum"),
    ]
}

fn try_use_prebuilt(
    lib_filename: &str,
    bridge_dir: &Path,
    workspace_root: &Path,
    output_path: &PathBuf,
    header_path: &PathBuf,
) -> bool {
    // Get modification times of all Go source files.
    // If any source is newer than a prebuilt library, we must rebuild.
    let newest_source_time = tracked_go_paths(bridge_dir)
        .iter()
        .filter_map(|p| p.metadata().ok())
        .filter_map(|m| m.modified().ok())
        .max();

    let prebuilt_locations = [
        // Nix flake puts prebuilt artifacts in workspace target/
        (
            workspace_root.join("target/debug").join(lib_filename),
            workspace_root.join("target/debug/libcue_bridge.h"),
        ),
        (
            workspace_root.join("target/release").join(lib_filename),
            workspace_root.join("target/release/libcue_bridge.h"),
        ),
        // Local development builds
        (
            bridge_dir.join("target/debug").join(lib_filename),
            bridge_dir.join("target/debug/libcue_bridge.h"),
        ),
        (
            bridge_dir.join("target/release").join(lib_filename),
            bridge_dir.join("target/release/libcue_bridge.h"),
        ),
        // Environment variable override (useful for CI/Nix)
        (
            PathBuf::from(env::var("CUE_BRIDGE_PATH").unwrap_or_default())
                .join("debug")
                .join(lib_filename),
            PathBuf::from(env::var("CUE_BRIDGE_PATH").unwrap_or_default())
                .join("debug/libcue_bridge.h"),
        ),
        (
            PathBuf::from(env::var("CUE_BRIDGE_PATH").unwrap_or_default())
                .join("release")
                .join(lib_filename),
            PathBuf::from(env::var("CUE_BRIDGE_PATH").unwrap_or_default())
                .join("release/libcue_bridge.h"),
        ),
    ];

    // Prefer release, then debug
    for (lib_path, header_path_candidate) in &prebuilt_locations {
        if lib_path.is_file()
            && header_path_candidate.is_file()
            && !lib_path.to_string_lossy().is_empty()
        {
            // Check if prebuilt is newer than all source files
            // If sources are newer, skip this prebuilt and rebuild from source
            if let Some(source_time) = newest_source_time
                && let Ok(lib_meta) = lib_path.metadata()
                && let Ok(lib_time) = lib_meta.modified()
                && lib_time < source_time
            {
                println!(
                    "cargo:warning=Prebuilt {} is older than source files, will rebuild",
                    lib_path.display()
                );
                continue; // Skip this prebuilt, try next or fall through to rebuild
            }

            // Remove destination files if they exist (might be read-only)
            let _ = std::fs::remove_file(output_path);
            let _ = std::fs::remove_file(header_path);

            std::fs::copy(lib_path, output_path).unwrap_or_else(|e| {
                panic!(
                    "Failed to copy pre-built bridge from {}: {}",
                    lib_path.display(),
                    e
                )
            });
            std::fs::copy(header_path_candidate, header_path).unwrap_or_else(|e| {
                panic!(
                    "Failed to copy pre-built header from {}: {}",
                    header_path_candidate.display(),
                    e
                )
            });

            let build_type = if lib_path.to_string_lossy().contains("release") {
                "release"
            } else {
                "debug"
            };
            println!(
                "Using pre-built Go bridge ({}) from: {}",
                build_type,
                lib_path.display()
            );
            return true;
        }
    }

    false
}

fn build_go_bridge(bridge_dir: &Path, output_path: &Path, target_triple: &str) {
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
                    panic!("Unsupported cross-compilation architecture: {target_triple}");
                };

                let zig_target = format!("{zig_arch}-linux-gnu");

                println!(
                    "cargo:warning=Configuring Zig cross-compilation toolchain for {zig_target}"
                );
                cmd.env("CC", format!("zig cc -target {zig_target}"));
                cmd.env("CXX", format!("zig c++ -target {zig_target}"));
                cmd.env("AR", "zig ar");
            } else {
                panic!(
                    "Cross-compiling from {host_triple} to {target_triple} requires Zig.\n\
                     Install Zig (https://ziglang.org/download/) or set CUE_BRIDGE_PATH."
                );
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

    let output_str = output_path
        .to_str()
        .expect("Failed to convert output path to string");

    cmd.args(["-buildmode=c-archive", "-o", output_str, "bridge.go"]);

    println!("Running Go command: {cmd:?}");

    let output = cmd
        .output()
        .expect("Failed to execute Go command. Make sure Go is installed.");

    if !output.status.success() {
        println!("Go build failed!");
        println!("stdout: {}", String::from_utf8_lossy(&output.stdout));
        println!("stderr: {}", String::from_utf8_lossy(&output.stderr));
        panic!("Failed to build libcue bridge");
    }

    println!("Go build completed successfully");
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
