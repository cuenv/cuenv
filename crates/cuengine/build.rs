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

    let output_path = out_dir.join("libcue_bridge.a");
    let header_path = out_dir.join("libcue_bridge.h");

    // Check for pre-built bridge first (Nix builds with pre-compiled Go bridge)
    let prebuilt_debug = bridge_dir.join("target/debug/libcue_bridge.a");
    let prebuilt_release = bridge_dir.join("target/release/libcue_bridge.a");
    let prebuilt_debug_header = bridge_dir.join("target/debug/libcue_bridge.h");
    let prebuilt_release_header = bridge_dir.join("target/release/libcue_bridge.h");

    if prebuilt_release.exists() && prebuilt_release_header.exists() {
        // Use pre-built release version
        std::fs::copy(&prebuilt_release, &output_path)
            .expect("Failed to copy pre-built release bridge");
        std::fs::copy(&prebuilt_release_header, &header_path)
            .expect("Failed to copy pre-built release header");
        println!("Using pre-built Go bridge (release)");
    } else if prebuilt_debug.exists() && prebuilt_debug_header.exists() {
        // Use pre-built debug version
        std::fs::copy(&prebuilt_debug, &output_path)
            .expect("Failed to copy pre-built debug bridge");
        std::fs::copy(&prebuilt_debug_header, &header_path)
            .expect("Failed to copy pre-built debug header");
        println!("Using pre-built Go bridge (debug)");
    } else {
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
    let target = env::var("TARGET")
        .unwrap_or_else(|_| env::var("HOST").expect("Neither TARGET nor HOST set by cargo"));

    if target.contains("windows") {
        // Windows-specific libraries
        println!("cargo:rustc-link-lib=ws2_32");
        println!("cargo:rustc-link-lib=userenv");
        println!("cargo:rustc-link-lib=ntdll");
        println!("cargo:rustc-link-lib=winmm");
    } else {
        // Unix-like systems
        println!("cargo:rustc-link-lib=pthread");
        println!("cargo:rustc-link-lib=m");
        println!("cargo:rustc-link-lib=dl");

        if target.contains("apple") || target.contains("darwin") {
            // macOS requires Security framework for certificate validation
            println!("cargo:rustc-link-lib=framework=Security");
            println!("cargo:rustc-link-lib=framework=CoreFoundation");
            println!("cargo:rustc-link-lib=framework=SystemConfiguration");
        }
    }
}
