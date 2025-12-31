//! Nix CLI command wrappers.
//!
//! Provides a clean abstraction layer around the `nix` command-line tool
//! for profile management and package operations.

use cuenv_core::Result;
use std::path::Path;
use std::process::Stdio;
use tokio::process::Command;
use tracing::debug;

/// Install a flake package into a profile.
///
/// # Errors
///
/// Returns an error if the nix command fails or the package cannot be installed.
pub async fn profile_install(profile_path: &Path, flake_ref: &str) -> Result<()> {
    debug!(%flake_ref, profile = ?profile_path, "Installing package into profile");

    let output = Command::new("nix")
        .args([
            "profile",
            "install",
            "--profile",
            profile_path.to_str().unwrap_or_default(),
            flake_ref,
        ])
        .output()
        .await
        .map_err(|e| cuenv_core::Error::tool_resolution(format!("Failed to run nix: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(cuenv_core::Error::tool_resolution(format!(
            "nix profile install failed for {flake_ref}: {stderr}"
        )));
    }

    Ok(())
}

/// List packages in a profile (returns JSON).
///
/// Returns the JSON output from `nix profile list --json`.
/// If the profile doesn't exist, returns an empty JSON object.
///
/// # Errors
///
/// Returns an error if the nix command fails unexpectedly.
pub async fn profile_list(profile_path: &Path) -> Result<String> {
    debug!(profile = ?profile_path, "Listing profile packages");

    let output = Command::new("nix")
        .args([
            "profile",
            "list",
            "--profile",
            profile_path.to_str().unwrap_or_default(),
            "--json",
        ])
        .output()
        .await
        .map_err(|e| cuenv_core::Error::tool_resolution(format!("Failed to run nix: {e}")))?;

    // Profile might not exist yet, which is fine
    if !output.status.success() {
        return Ok("{}".to_string());
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Remove a package from profile by index.
///
/// # Errors
///
/// Returns an error if the nix command fails.
#[allow(dead_code)]
pub async fn profile_remove(profile_path: &Path, index: u32) -> Result<()> {
    debug!(profile = ?profile_path, %index, "Removing package from profile");

    let output = Command::new("nix")
        .args([
            "profile",
            "remove",
            "--profile",
            profile_path.to_str().unwrap_or_default(),
            &index.to_string(),
        ])
        .output()
        .await
        .map_err(|e| cuenv_core::Error::tool_resolution(format!("Failed to run nix: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(cuenv_core::Error::tool_resolution(format!(
            "nix profile remove failed: {stderr}"
        )));
    }

    Ok(())
}

/// Check if nix is available with flakes enabled.
pub async fn check_available() -> bool {
    Command::new("nix")
        .args(["flake", "--help"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_check_available() {
        // This test just verifies the function doesn't panic
        // Result depends on whether nix is installed
        let _ = check_available().await;
    }
}
