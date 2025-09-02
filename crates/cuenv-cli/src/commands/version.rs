use std::env;
use tracing::instrument;

#[instrument]
pub fn get_version_info() -> String {
    let version = env!("CARGO_PKG_VERSION");
    let name = env!("CARGO_PKG_NAME");
    let authors = env!("CARGO_PKG_AUTHORS");
    let description = env!("CARGO_PKG_DESCRIPTION");

    tracing::debug!(
        package_name = name,
        package_version = version,
        "Gathering package information"
    );

    // Get build information if available
    let target = env::var("TARGET").unwrap_or_else(|_| "unknown".to_string());
    let rustc_version = env::var("RUSTC_VERSION").unwrap_or_else(|_| "unknown".to_string());
    let build_date = env::var("BUILD_DATE").unwrap_or_else(|_| "unknown".to_string());

    tracing::debug!(
        target = %target,
        rustc_version = %rustc_version,
        build_date = %build_date,
        "Gathered build information"
    );

    let version_info = format!(
        "{} {} - {}\n\
        Authors: {}\n\
        Target: {}\n\
        Rust Compiler: {}\n\
        Build Date: {}\n\
        Correlation ID: {}\n\
        \n\
        cuenv is an event-driven CLI with enhanced tracing and miette diagnostics.",
        name,
        version,
        description,
        authors,
        target,
        rustc_version,
        build_date,
        crate::tracing::correlation_id()
    );

    tracing::info!(
        version_info_length = version_info.len(),
        "Version information compiled"
    );

    version_info
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_version_info_format() {
        let version_info = get_version_info();

        // Test that all expected components are present
        assert!(version_info.contains("cuenv-cli"));
        assert!(version_info.contains("0.1.0"));
        assert!(version_info.contains("Authors:"));
        assert!(version_info.contains("Target:"));
        assert!(version_info.contains("Rust Compiler:"));
        assert!(version_info.contains("Build Date:"));
        assert!(version_info.contains("Correlation ID:"));
        assert!(version_info.contains("cuenv is an event-driven CLI"));
    }

    #[test]
    fn test_get_version_info_not_empty() {
        let version_info = get_version_info();
        assert!(!version_info.is_empty());
        assert!(version_info.len() > 100); // Should be reasonably long
    }

    #[test]
    fn test_get_version_info_correlation_id_format() {
        let version_info = get_version_info();

        // Check that correlation ID is present and looks like a UUID
        assert!(version_info.contains("Correlation ID:"));

        // Extract the line with correlation ID
        let correlation_line = version_info
            .lines()
            .find(|line| line.contains("Correlation ID:"))
            .expect("Should contain correlation ID");

        // Should have the format "Correlation ID: <uuid>"
        let parts: Vec<&str> = correlation_line.split(':').collect();
        assert_eq!(parts.len(), 2);
        let uuid_part = parts[1].trim();
        assert_eq!(uuid_part.len(), 36); // UUID length
        assert!(uuid_part.contains('-')); // UUIDs have hyphens
    }

    #[test]
    fn test_get_version_info_consistency() {
        // Test that multiple calls return consistent format (though correlation ID may differ)
        let version1 = get_version_info();
        let version2 = get_version_info();

        // Split by correlation ID and compare everything else
        let version1_parts: Vec<&str> = version1.split("Correlation ID:").collect();
        let version2_parts: Vec<&str> = version2.split("Correlation ID:").collect();

        // Everything before correlation ID should be identical
        assert_eq!(version1_parts[0], version2_parts[0]);

        // Everything after correlation ID should be identical (the description part)
        let desc1 = version1_parts[1].split('\n').skip(2).collect::<Vec<_>>();
        let desc2 = version2_parts[1].split('\n').skip(2).collect::<Vec<_>>();
        assert_eq!(desc1, desc2);
    }
}
