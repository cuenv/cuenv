//! Build script for compiling the Go CUE bridge

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    // Skip entire build script on docs.rs - no Go toolchain available
    if env::var("DOCS_RS").is_ok() {
        println!("cargo:warning=Skipping Go FFI build for docs.rs");
        return;
    }

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=bridge.go");
    println!("cargo:rerun-if-changed=bridge.h");

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR not set by cargo"));
    let bridge_dir = PathBuf::from(".");

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

    // Try to use prebuilt artifacts first (produced by Nix/flake builds)
    let workspace_root =
        PathBuf::from(env::var("CARGO_WORKSPACE_DIR").unwrap_or_else(|_| "../..".to_string()));

    if !try_use_prebuilt(
        lib_filename,
        &bridge_dir,
        &workspace_root,
        &output_path,
        &header_path,
    ) {
        build_go_bridge(&bridge_dir, &output_path, &target_triple);

        // Verify the library was actually created
        assert!(
            output_path.exists(),
            "Go bridge library was not created at expected path: {}",
            output_path.display()
        );
        println!("Successfully created library at: {}", output_path.display());
    }

    configure_rustc_linking(&target_triple, &out_dir);
}

fn try_use_prebuilt(
    lib_filename: &str,
    bridge_dir: &Path,
    workspace_root: &Path,
    output_path: &PathBuf,
    header_path: &PathBuf,
) -> bool {
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
