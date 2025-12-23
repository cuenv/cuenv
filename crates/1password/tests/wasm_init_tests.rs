//! WASM initialization integration tests
//!
//! These tests verify that the 1Password WASM SDK can be loaded and the Extism
//! plugin can be initialized. This catches platform/runtime compatibility issues
//! that would otherwise only appear at secret resolution time.
//!
//! The tests download the WASM if not already present, so they run in CI without
//! requiring pre-setup.

use cuenv_1password::secrets::{core, wasm};
use std::path::PathBuf;

/// WASM SDK URL - must match the one in secrets.rs
const ONEPASSWORD_WASM_URL: &str =
    "https://github.com/1Password/onepassword-sdk-go/raw/refs/tags/v0.3.1/internal/wasm/core.wasm";

/// Ensure WASM is available, downloading if necessary
fn ensure_wasm_available() -> PathBuf {
    let path = wasm::onepassword_wasm_path().expect("Should get cache path");

    if !path.exists() {
        let response = reqwest::blocking::get(ONEPASSWORD_WASM_URL)
            .expect("WASM download request should succeed");

        assert!(
            response.status().is_success(),
            "WASM download failed with status: {}",
            response.status()
        );

        let bytes = response.bytes().expect("Should read response bytes");

        std::fs::create_dir_all(path.parent().unwrap()).expect("Should create cache dir");
        std::fs::write(&path, &bytes).expect("Should write WASM file");
    }

    path
}

/// Test that the WASM file can be loaded and the Extism plugin initializes
///
/// This is the critical test - it verifies that the Extism runtime can load
/// the 1Password WASM SDK on this platform with the required host functions.
/// This is where CI failures occur if there's a platform compatibility issue.
#[test]
fn test_wasm_loads_and_plugin_initializes() {
    let path = ensure_wasm_available();

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
#[test]
fn test_shared_core_initializes() {
    let _path = ensure_wasm_available();

    // This tests the full SharedCore initialization path
    let core_mutex =
        core::SharedCore::get_or_init().expect("SharedCore should initialize successfully");

    // Verify we can acquire the lock
    let guard = core_mutex.lock().expect("Should acquire lock");
    assert!(guard.is_some(), "SharedCore should be initialized");
}
