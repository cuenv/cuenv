use super::*;

#[test]
fn test_panic_hook() {
    // Test that panic hook is properly set
    // Note: We can't easily test the panic hook directly
    // Just verify that we can set and take a hook
    let _ = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let _ = std::panic::take_hook();
    // Test passes if no panic occurs
}

#[test]
fn test_cli_args_json_flag() {
    let cli_args = ["cuenv".to_string(), "--json".to_string()];
    let json_flag = cli_args.iter().any(|arg| arg == "--json");
    assert!(json_flag);
}

#[test]
fn test_cli_args_level_flag() {
    let cli_args = [
        "cuenv".to_string(),
        "--level".to_string(),
        "debug".to_string(),
    ];
    let level_flag = cli_args.windows(2).find_map(|args| {
        if args[0] == "--level" || args[0] == "-l" {
            Some(args[1].as_str())
        } else {
            None
        }
    });
    assert_eq!(level_flag, Some("debug"));
}

#[test]
fn test_trace_format_selection() {
    let json_flag = true;
    let trace_format = if json_flag {
        TracingFormat::Json
    } else {
        TracingFormat::Pretty
    };
    assert!(matches!(trace_format, TracingFormat::Json));

    let json_flag = false;
    let trace_format = if json_flag {
        TracingFormat::Json
    } else {
        TracingFormat::Pretty
    };
    assert!(matches!(trace_format, TracingFormat::Pretty));
}

#[test]
fn test_log_level_parsing() {
    let test_cases = vec![
        (Some("trace"), Level::TRACE),
        (Some("debug"), Level::DEBUG),
        (Some("info"), Level::INFO),
        (Some("warn"), Level::WARN),
        (Some("error"), Level::ERROR),
        (None, Level::WARN),            // Default
        (Some("invalid"), Level::WARN), // Invalid falls back to default
    ];

    for (input, expected) in test_cases {
        let log_level = match input {
            Some("trace") => Level::TRACE,
            Some("debug") => Level::DEBUG,
            Some("info") => Level::INFO,
            Some("error") => Level::ERROR,
            _ => Level::WARN,
        };
        assert_eq!(log_level, expected);
    }
}

#[test]
fn test_tracing_config_default() {
    let tracing_config = TracingConfig {
        format: TracingFormat::Dev,
        level: Level::WARN,
        ..Default::default()
    };
    assert!(matches!(tracing_config.format, TracingFormat::Dev));
    assert_eq!(tracing_config.level, Level::WARN);
}

#[tokio::test]
async fn test_command_conversion() {
    use cli::{Commands, OutputFormat};

    // Test Version command conversion
    let cli_command = Commands::Version {
        output_format: OutputFormat::Text,
    };
    let command: Command = cli_command.into_command(None);
    match command {
        Command::Version { format } => assert_eq!(format, "text"),
        _ => panic!("Expected Command::Version"),
    }
}

/// Build a synthetic lockfile with a single image artifact carrying the
/// provided `extract` entries for `platform_str`.
fn make_oci_lockfile(
    image: &str,
    digest: &str,
    platform_str: &str,
    extract: Vec<cuenv_core::lockfile::LockedOciExtract>,
) -> cuenv_core::lockfile::Lockfile {
    use cuenv_core::lockfile::{ArtifactKind, LockedArtifact, Lockfile, PlatformData};
    use std::collections::BTreeMap;

    let mut lockfile = Lockfile::new();
    lockfile.artifacts.push(LockedArtifact {
        kind: ArtifactKind::Image {
            image: image.to_string(),
            extract,
        },
        platforms: BTreeMap::from([(
            platform_str.to_string(),
            PlatformData {
                digest: digest.to_string(),
                size: None,
            },
        )]),
    });
    lockfile
}

#[tokio::test]
async fn test_activate_lockfile_artifacts_cache_hit_path() {
    use cuenv_core::lockfile::LockedOciExtract;
    use cuenv_tools_oci::{OciCache, OciClient, Platform};

    let tmp = tempfile::tempdir().unwrap();
    let cache = OciCache::new(tmp.path().to_path_buf());
    cache.ensure_dirs().unwrap();

    let digest = "sha256:cafebabe";
    let binary_name = "nginx";

    // Pre-stage the binary in the cache so activate_lockfile_artifacts
    // hits the fast path and never touches the network.
    let staged_source = tmp.path().join("source-nginx");
    std::fs::write(&staged_source, b"#!/bin/sh\necho nginx\n").unwrap();
    let staged = cache
        .store_binary(digest, binary_name, &staged_source)
        .unwrap();
    assert!(staged.exists());

    let platform = Platform::new("linux", "x86_64");
    let lockfile = make_oci_lockfile(
        "nginx:1.25-alpine",
        digest,
        &platform.to_string(),
        vec![LockedOciExtract {
            path: "/usr/sbin/nginx".to_string(),
            as_name: None,
        }],
    );

    let client = OciClient::new();
    let bin_dirs = super::activate_lockfile_artifacts(&lockfile, &client, &cache, &platform)
        .await
        .expect("activation should succeed via cache hit");

    let expected_parent = cache.binary_path(digest, binary_name);
    let expected_parent = expected_parent.parent().unwrap().to_path_buf();
    assert!(
        bin_dirs.contains(&expected_parent),
        "bin_dirs ({bin_dirs:?}) should contain {expected_parent:?}"
    );
    assert_eq!(bin_dirs.len(), 1);
}

#[tokio::test]
async fn test_activate_lockfile_artifacts_skips_when_no_extract() {
    use cuenv_tools_oci::{OciCache, OciClient, Platform};

    let tmp = tempfile::tempdir().unwrap();
    let cache = OciCache::new(tmp.path().to_path_buf());
    cache.ensure_dirs().unwrap();

    let platform = Platform::new("linux", "x86_64");
    // No extract entries — should warn + skip without contacting any
    // network and return an empty set of PATH directories.
    let lockfile = make_oci_lockfile(
        "nginx:1.25-alpine",
        "sha256:cafebabe",
        &platform.to_string(),
        vec![],
    );

    let client = OciClient::new();
    let bin_dirs = super::activate_lockfile_artifacts(&lockfile, &client, &cache, &platform)
        .await
        .expect("activation should succeed by skipping the artifact");

    assert!(bin_dirs.is_empty());
}

#[tokio::test]
async fn test_activate_lockfile_artifacts_skips_when_platform_mismatch() {
    use cuenv_core::lockfile::LockedOciExtract;
    use cuenv_tools_oci::{OciCache, OciClient, Platform};

    let tmp = tempfile::tempdir().unwrap();
    let cache = OciCache::new(tmp.path().to_path_buf());
    cache.ensure_dirs().unwrap();

    // Lockfile only has darwin-arm64 data; we activate on linux-x86_64.
    let lockfile = make_oci_lockfile(
        "nginx:1.25-alpine",
        "sha256:cafebabe",
        "darwin-arm64",
        vec![LockedOciExtract {
            path: "/usr/sbin/nginx".to_string(),
            as_name: None,
        }],
    );

    let platform = Platform::new("linux", "x86_64");
    let client = OciClient::new();
    let bin_dirs = super::activate_lockfile_artifacts(&lockfile, &client, &cache, &platform)
        .await
        .expect("should succeed by skipping non-matching platform");

    assert!(bin_dirs.is_empty());
}
