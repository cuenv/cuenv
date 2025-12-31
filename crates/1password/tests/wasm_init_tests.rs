//! WASM initialization integration tests
//!
//! These tests verify that the 1Password WASM SDK can be loaded and the Extism
//! plugin can be initialized. This catches platform/runtime compatibility issues
//! that would otherwise only appear at secret resolution time.
//!
//! The WASM is automatically downloaded if not present in the cache.

// Integration tests can use unwrap/expect for cleaner assertions
#![allow(clippy::expect_used)]

use cuenv_1password::secrets::{core, wasm};
use std::path::PathBuf;

/// 1Password WASM SDK URL (pinned to v0.3.1)
const ONEPASSWORD_WASM_URL: &str =
    "https://github.com/1Password/onepassword-sdk-go/raw/refs/tags/v0.3.1/internal/wasm/core.wasm";

/// Minimum expected size for the 1Password WASM file (5 MB)
const MIN_WASM_SIZE: u64 = 5_000_000;

/// Ensure WASM is available, downloading if necessary.
/// Uses atomic file operations to handle concurrent test execution safely.
#[allow(clippy::print_stderr)]
fn ensure_wasm_available() -> PathBuf {
    let path = wasm::onepassword_wasm_path().expect("Should get WASM path");

    // Check if file already exists with valid size (another test may have downloaded it)
    if let Ok(metadata) = std::fs::metadata(&path)
        && metadata.len() >= MIN_WASM_SIZE
    {
        return path;
    }

    // Create parent directory
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("Should create cache directory");
    }

    // Use a temp file with process ID to avoid conflicts between parallel tests
    let temp_path = path.with_extension(format!("wasm.tmp.{}", std::process::id()));

    eprintln!("Downloading 1Password WASM SDK to {}...", path.display());

    // Download using reqwest blocking client
    let response = reqwest::blocking::get(ONEPASSWORD_WASM_URL).expect("Should download WASM");
    assert!(
        response.status().is_success(),
        "Failed to download WASM: HTTP {}",
        response.status()
    );

    let bytes = response.bytes().expect("Should read response body");
    assert!(
        bytes.len() as u64 >= MIN_WASM_SIZE,
        "Downloaded WASM too small: {} bytes",
        bytes.len()
    );

    // Write to temp file first
    std::fs::write(&temp_path, &bytes).expect("Should write temp WASM file");

    // Atomically rename to final path (handles concurrent downloads safely)
    // If another process already created the file, this may fail on some platforms,
    // but the file should still be valid from the other process
    if let Err(e) = std::fs::rename(&temp_path, &path) {
        eprintln!("Rename failed (likely concurrent download): {e}");
        // Clean up temp file
        let _ = std::fs::remove_file(&temp_path);
    }

    eprintln!("Downloaded {} bytes", bytes.len());

    // Verify final file exists and has valid size
    let final_metadata = std::fs::metadata(&path).expect("WASM file should exist after download");
    assert!(
        final_metadata.len() >= MIN_WASM_SIZE,
        "Final WASM file too small: {} bytes",
        final_metadata.len()
    );

    path
}

/// Test that the WASM file can be loaded and the Extism plugin initializes
///
/// This is the critical test - it verifies that the Extism runtime can load
/// the 1Password WASM SDK on this platform with the required host functions.
/// This is where CI failures occur if there's a platform compatibility issue.
///
/// Note: Uses unsafe to set HOME env var for wasmtime's bytecode cache.
/// In Nix sandbox, HOME=/homeless-shelter which is unwritable.
///
#[test]
#[allow(unsafe_code)]
fn test_wasm_loads_and_plugin_initializes() {
    let path = ensure_wasm_available();

    // Set HOME to a temp directory for wasmtime's bytecode cache.
    let temp_home = std::env::temp_dir().join("cuenv-wasm-test-home");
    std::fs::create_dir_all(&temp_home).expect("Should create temp home");
    // SAFETY: This test runs in isolation and no other threads are reading
    // environment variables concurrently during this initialization.
    unsafe { std::env::set_var("HOME", &temp_home) };

    // Load WASM bytes
    let bytes = std::fs::read(&path).expect("Should read WASM file");

    // Sanity check - the WASM should be several MB
    let len = bytes.len();
    assert!(
        len > 1_000_000,
        "WASM should be > 1MB (got {len} bytes) - file may be corrupt or incomplete"
    );

    // Create Extism manifest with allowed hosts (same as core.rs)
    let manifest = extism::Manifest::new([extism::Wasm::data(bytes)]).with_allowed_hosts(
        ["*.1password.com", "*.1password.ca", "*.1password.eu"]
            .into_iter()
            .map(String::from),
    );

    // Get the host functions required by the 1Password WASM SDK
    let host_functions = core::create_host_functions();

    // Initialize plugin with host functions - THIS IS THE CRITICAL TEST
    // This is where failures occur if the WASM is incompatible with the platform
    let plugin = extism::Plugin::new(&manifest, host_functions, true)
        .expect("Extism plugin initialization should succeed");

    // Verify the expected function exports exist
    // These are the functions called by the SharedCore
    assert!(
        plugin.function_exists("init_client"),
        "Plugin should export init_client function"
    );
    assert!(
        plugin.function_exists("invoke"),
        "Plugin should export invoke function"
    );
    assert!(
        plugin.function_exists("release_client"),
        "Plugin should export release_client function"
    );
}

/// Test that WASM size is reasonable (catches incomplete downloads)
#[test]
fn test_wasm_file_size() {
    let path = ensure_wasm_available();
    let metadata = std::fs::metadata(&path).expect("Should get file metadata");

    // The WASM file should be around 8-10 MB
    // Precision loss is acceptable for display purposes
    #[expect(clippy::cast_precision_loss)]
    let size_mb = metadata.len() as f64 / (1024.0 * 1024.0);

    assert!(
        size_mb > 5.0 && size_mb < 20.0,
        "WASM file should be 5-20 MB (got {size_mb:.2} MB)"
    );
}

/// Test using `SharedCore` directly (same code path as production)
///
/// Note: Uses unsafe to set HOME and ONEPASSWORD_WASM_PATH env vars.
/// HOME is changed for wasmtime's bytecode cache (in Nix sandbox, HOME=/homeless-shelter).
/// ONEPASSWORD_WASM_PATH ensures SharedCore finds the WASM after HOME is changed.
#[test]
#[allow(unsafe_code, clippy::significant_drop_tightening)]
fn test_shared_core_initializes() {
    let wasm_path = ensure_wasm_available();

    // SAFETY: This test runs in isolation and no other threads are reading
    // environment variables concurrently during this initialization.

    // Set ONEPASSWORD_WASM_PATH before changing HOME, so SharedCore can find the WASM
    // (changing HOME affects dirs::cache_dir() which onepassword_wasm_path() uses).
    unsafe { std::env::set_var("ONEPASSWORD_WASM_PATH", &wasm_path) };

    // Set HOME to a temp directory for wasmtime's bytecode cache.
    let temp_home = std::env::temp_dir().join("cuenv-wasm-test-home");
    std::fs::create_dir_all(&temp_home).expect("Should create temp home");
    unsafe { std::env::set_var("HOME", &temp_home) };

    // This tests the full SharedCore initialization path
    let core_mutex =
        core::SharedCore::get_or_init().expect("SharedCore should initialize successfully");

    // Verify we can acquire the lock
    let guard = core_mutex.lock().expect("Should acquire lock");
    assert!(guard.is_some(), "SharedCore should be initialized");
}
