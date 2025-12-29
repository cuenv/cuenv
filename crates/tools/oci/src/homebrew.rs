//! Homebrew formula metadata and dependency resolution.
//!
//! Fetches formula information from the Homebrew API and resolves
//! transitive dependencies for proper library path setup.

use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use tracing::{debug, info};

use crate::{Error, Result};

/// Homebrew formula metadata from the API.
#[derive(Debug, Clone, Deserialize)]
pub struct HomebrewFormula {
    /// Formula name (e.g., "jq").
    pub name: String,
    /// Version information.
    pub versions: Versions,
    /// Runtime dependencies.
    #[serde(default)]
    pub dependencies: Vec<String>,
    /// Bottle (pre-built binary) information.
    pub bottle: Option<Bottle>,
}

/// Version information for a formula.
#[derive(Debug, Clone, Deserialize)]
pub struct Versions {
    /// Stable version string.
    pub stable: String,
}

/// Bottle information for pre-built binaries.
#[derive(Debug, Clone, Deserialize)]
pub struct Bottle {
    /// Stable bottle configuration.
    pub stable: BottleStable,
}

/// Stable bottle files by platform.
#[derive(Debug, Clone, Deserialize)]
pub struct BottleStable {
    /// Platform-specific bottle files.
    pub files: HashMap<String, BottleFile>,
}

/// Individual bottle file information.
#[derive(Debug, Clone, Deserialize)]
pub struct BottleFile {
    /// URL to the bottle blob (usually ghcr.io).
    pub url: String,
    /// SHA256 checksum of the bottle.
    pub sha256: String,
}

impl HomebrewFormula {
    /// Get the bottle file for a specific platform.
    ///
    /// Platform names are like "arm64_sonoma", "x86_64_linux", etc.
    /// Falls back to "all" for platform-independent packages (e.g., node-based tools).
    #[must_use]
    pub fn get_bottle(&self, platform: &str) -> Option<&BottleFile> {
        self.bottle.as_ref().and_then(|b| {
            b.stable
                .files
                .get(platform)
                .or_else(|| b.stable.files.get("all"))
        })
    }

    /// Get all available bottle platforms.
    #[must_use]
    pub fn available_platforms(&self) -> Vec<&str> {
        self.bottle
            .as_ref()
            .map(|b| b.stable.files.keys().map(String::as_str).collect())
            .unwrap_or_default()
    }
}

/// Fetch formula metadata from the Homebrew API.
///
/// # Arguments
///
/// * `name` - Formula name (e.g., "jq", "oniguruma")
///
/// # Errors
///
/// Returns an error if the API request fails or the formula doesn't exist.
pub async fn fetch_formula(name: &str) -> Result<HomebrewFormula> {
    let url = format!("https://formulae.brew.sh/api/formula/{}.json", name);
    debug!(%name, %url, "Fetching Homebrew formula");

    let response = reqwest::get(&url)
        .await
        .map_err(|e| Error::Homebrew(format!("Failed to fetch formula '{}': {}", name, e)))?;

    if !response.status().is_success() {
        return Err(Error::Homebrew(format!(
            "Formula '{}' not found (HTTP {})",
            name,
            response.status()
        )));
    }

    let formula: HomebrewFormula = response
        .json()
        .await
        .map_err(|e| Error::Homebrew(format!("Failed to parse formula '{}': {}", name, e)))?;

    debug!(
        name = %formula.name,
        version = %formula.versions.stable,
        deps = ?formula.dependencies,
        "Fetched formula"
    );

    Ok(formula)
}

/// Resolve a formula with all its transitive dependencies.
///
/// Returns a list of formulas in dependency order (dependencies first).
///
/// # Arguments
///
/// * `name` - Root formula name
///
/// # Errors
///
/// Returns an error if any formula cannot be fetched.
pub async fn resolve_with_deps(name: &str) -> Result<Vec<HomebrewFormula>> {
    info!(%name, "Resolving formula with dependencies");

    let mut resolved = Vec::new();
    let mut queue = vec![name.to_string()];
    let mut seen = HashSet::new();

    while let Some(formula_name) = queue.pop() {
        if seen.contains(&formula_name) {
            continue;
        }
        seen.insert(formula_name.clone());

        let formula = fetch_formula(&formula_name).await?;

        // Queue dependencies for resolution
        for dep in &formula.dependencies {
            if !seen.contains(dep) {
                queue.push(dep.clone());
            }
        }

        resolved.push(formula);
    }

    // Reverse so dependencies come before dependents
    resolved.reverse();

    info!(
        root = %name,
        total = resolved.len(),
        formulas = ?resolved.iter().map(|f| &f.name).collect::<Vec<_>>(),
        "Resolved dependency tree"
    );

    Ok(resolved)
}

/// Extract formula name from a Homebrew image reference.
///
/// # Arguments
///
/// * `image` - Image reference (e.g., "ghcr.io/homebrew/core/jq:1.7.1")
///
/// # Returns
///
/// The formula name if the image is a Homebrew reference, None otherwise.
///
/// # Examples
///
/// ```
/// use cuenv_tools_oci::formula_name_from_image;
///
/// assert_eq!(formula_name_from_image("ghcr.io/homebrew/core/jq:1.7.1"), Some("jq".to_string()));
/// assert_eq!(formula_name_from_image("nginx:1.25"), None);
/// ```
#[must_use]
pub fn formula_name_from_image(image: &str) -> Option<String> {
    if !image.starts_with("ghcr.io/homebrew/") {
        return None;
    }

    // ghcr.io/homebrew/core/jq:1.7.1 -> ["ghcr.io", "homebrew", "core", "jq:1.7.1"]
    let parts: Vec<&str> = image.split('/').collect();
    if parts.len() >= 4 {
        // "jq:1.7.1" -> "jq"
        Some(parts[3].split(':').next()?.to_string())
    } else {
        None
    }
}

/// Convert a cuenv platform string to Homebrew bottle platform suffix.
///
/// # Arguments
///
/// * `platform` - Platform string (e.g., "darwin-arm64", "linux-x86_64")
///
/// # Returns
///
/// The Homebrew platform suffix if mappable, None otherwise.
///
/// Note: We use `arm64_sonoma` for darwin-arm64 as it's the most common.
/// Homebrew also has `arm64_sequoia`, `arm64_tahoe`, `arm64_ventura`.
#[must_use]
pub fn to_homebrew_platform(platform: &str) -> Option<String> {
    match platform {
        "darwin-arm64" => Some("arm64_sonoma".to_string()),
        "darwin-x86_64" => Some("sonoma".to_string()),
        "linux-x86_64" => Some("x86_64_linux".to_string()),
        "linux-arm64" => Some("arm64_linux".to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_formula_name_from_image() {
        assert_eq!(
            formula_name_from_image("ghcr.io/homebrew/core/jq:1.7.1"),
            Some("jq".to_string())
        );
        assert_eq!(
            formula_name_from_image("ghcr.io/homebrew/core/ripgrep:14.1.0"),
            Some("ripgrep".to_string())
        );
        assert_eq!(formula_name_from_image("nginx:1.25"), None);
        assert_eq!(formula_name_from_image("ghcr.io/other/image:tag"), None);
    }

    #[test]
    fn test_to_homebrew_platform() {
        assert_eq!(
            to_homebrew_platform("darwin-arm64"),
            Some("arm64_sonoma".to_string())
        );
        assert_eq!(
            to_homebrew_platform("darwin-x86_64"),
            Some("sonoma".to_string())
        );
        assert_eq!(
            to_homebrew_platform("linux-x86_64"),
            Some("x86_64_linux".to_string())
        );
        assert_eq!(
            to_homebrew_platform("linux-arm64"),
            Some("arm64_linux".to_string())
        );
        assert_eq!(to_homebrew_platform("windows-x86_64"), None);
    }

    #[tokio::test]
    async fn test_fetch_formula_jq() {
        // Note: This test hits the real Homebrew API
        let formula = fetch_formula("jq").await.unwrap();
        assert_eq!(formula.name, "jq");
        assert!(!formula.versions.stable.is_empty());
        assert!(formula.dependencies.contains(&"oniguruma".to_string()));
    }
}
