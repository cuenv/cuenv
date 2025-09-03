//! Build script for compiling the Go CUE bridge

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
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

    // Check for pre-built bridge first (Nix builds with pre-compiled Go bridge)
    // Try multiple locations: workspace root and current dir
    let workspace_root =
        PathBuf::from(env::var("CARGO_WORKSPACE_DIR").unwrap_or_else(|_| "../..".to_string()));

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

    let mut found_prebuilt = false;

    // Try to find prebuilt bridge in order of preference (release first, then debug)
    for (lib_path, header_path_candidate) in prebuilt_locations.iter() {
        if lib_path.exists()
            && header_path_candidate.exists()
            && !lib_path.to_string_lossy().is_empty()
        {
            std::fs::copy(lib_path, &output_path).unwrap_or_else(|e| {
                panic!(
                    "Failed to copy pre-built bridge from {}: {}",
                    lib_path.display(),
                    e
                )
            });
            std::fs::copy(header_path_candidate, &header_path).unwrap_or_else(|e| {
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
            found_prebuilt = true;
            break;
        }
    }

    if !found_prebuilt {
        // Build the Go shared library with CGO (fallback for non-Nix builds)
        println!("Building Go bridge from source");
        let mut cmd = Command::new("go");
        cmd.current_dir(&bridge_dir).arg("build");

        // Use vendor directory if it exists (for Nix builds)
        if bridge_dir.join("vendor").exists() {
            cmd.arg("-mod=vendor");
        }

        let output_str = output_path
            .to_str()
            .expect("Failed to convert output path to string");

        cmd.args(["-buildmode=c-archive", "-o", output_str, "bridge.go"]);

        let status = cmd
            .status()
            .expect("Failed to build Go shared library. Make sure Go is installed.");

        assert!(status.success(), "Failed to build libcue bridge");
    }

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
