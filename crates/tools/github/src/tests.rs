use super::*;
use temp_env::with_vars;
use tempfile::TempDir;

fn with_token_env<R>(
    github_token: Option<&str>,
    gh_token: Option<&str>,
    test: impl FnOnce() -> R,
) -> R {
    with_vars(
        [("GITHUB_TOKEN", github_token), ("GH_TOKEN", gh_token)],
        test,
    )
}

// ==========================================================================
// GitHubToolProvider construction and ToolProvider trait tests
// ==========================================================================

#[test]
fn test_provider_name() {
    let provider = GitHubToolProvider::new();
    assert_eq!(provider.name(), "github");
}

#[test]
fn test_provider_description() {
    let provider = GitHubToolProvider::new();
    assert_eq!(provider.description(), "Fetch tools from GitHub Releases");
}

#[test]
fn test_provider_default() {
    let provider = GitHubToolProvider::default();
    assert_eq!(provider.name(), "github");
}

#[test]
fn test_provider_new_defers_client_initialization() {
    let provider = GitHubToolProvider::new();
    assert!(provider.client.get().is_none());
}

#[test]
fn test_can_handle() {
    let provider = GitHubToolProvider::new();

    let github_source = ToolSource::GitHub {
        repo: "org/repo".into(),
        tag: "v1".into(),
        asset: "file.zip".into(),
        extract: vec![],
    };
    assert!(provider.can_handle(&github_source));

    let nix_source = ToolSource::Nix {
        flake: "nixpkgs".into(),
        package: "jq".into(),
        output: None,
    };
    assert!(!provider.can_handle(&nix_source));
}

#[test]
fn test_can_handle_github_with_path() {
    let provider = GitHubToolProvider::new();

    let source = ToolSource::GitHub {
        repo: "owner/repo".into(),
        tag: "v1.0.0".into(),
        asset: "archive.tar.gz".into(),
        extract: vec![ToolExtract::Bin {
            path: "bin/tool".into(),
            as_name: None,
        }],
    };
    assert!(provider.can_handle(&source));
}

// ==========================================================================
// expand_template tests
// ==========================================================================

#[test]
fn test_expand_template() {
    let provider = GitHubToolProvider::new();
    let platform = Platform::new(Os::Darwin, Arch::Arm64);

    assert_eq!(
        provider.expand_template("bun-{os}-{arch}.zip", "1.0.0", &platform),
        "bun-darwin-aarch64.zip"
    );

    assert_eq!(
        provider.expand_template("v{version}", "1.0.0", &platform),
        "v1.0.0"
    );
}

#[test]
fn test_expand_template_linux_x86_64() {
    let provider = GitHubToolProvider::new();
    let platform = Platform::new(Os::Linux, Arch::X86_64);

    assert_eq!(
        provider.expand_template("{os}-{arch}", "1.0.0", &platform),
        "linux-x86_64"
    );
}

#[test]
fn test_expand_template_all_placeholders() {
    let provider = GitHubToolProvider::new();
    let platform = Platform::new(Os::Darwin, Arch::X86_64);

    assert_eq!(
        provider.expand_template("tool-{version}-{os}-{arch}.zip", "2.5.1", &platform),
        "tool-2.5.1-darwin-x86_64.zip"
    );
}

#[test]
fn test_expand_template_no_placeholders() {
    let provider = GitHubToolProvider::new();
    let platform = Platform::new(Os::Linux, Arch::Arm64);

    assert_eq!(
        provider.expand_template("static-name.tar.gz", "1.0.0", &platform),
        "static-name.tar.gz"
    );
}

// ==========================================================================
// tool_cache_dir tests
// ==========================================================================

#[test]
fn test_tool_cache_dir() {
    let provider = GitHubToolProvider::new();
    let temp_dir = TempDir::new().unwrap();
    let options = ToolOptions::new().with_cache_dir(temp_dir.path().to_path_buf());

    let cache_dir = provider.tool_cache_dir(&options, "mytool", "1.2.3");

    assert!(cache_dir.ends_with("github/mytool/1.2.3"));
    assert!(cache_dir.starts_with(temp_dir.path()));
}

#[test]
fn test_tool_cache_dir_different_versions() {
    let provider = GitHubToolProvider::new();
    let temp_dir = TempDir::new().unwrap();
    let options = ToolOptions::new().with_cache_dir(temp_dir.path().to_path_buf());

    let cache_v1 = provider.tool_cache_dir(&options, "tool", "1.0.0");
    let cache_v2 = provider.tool_cache_dir(&options, "tool", "2.0.0");

    assert_ne!(cache_v1, cache_v2);
    assert!(cache_v1.ends_with("1.0.0"));
    assert!(cache_v2.ends_with("2.0.0"));
}

// ==========================================================================
// library path routing tests
// ==========================================================================

#[test]
fn test_path_looks_like_library() {
    assert!(GitHubToolProvider::path_looks_like_library(
        "lib/libfdb_c.dylib"
    ));
    assert!(GitHubToolProvider::path_looks_like_library(
        "lib/libssl.so.3"
    ));
    assert!(GitHubToolProvider::path_looks_like_library(
        "bin/sqlite3.dll"
    ));
    assert!(!GitHubToolProvider::path_looks_like_library("bin/fdbcli"));
}

#[test]
fn test_cache_targets_from_source_uses_lib_for_library_extract() {
    let provider = GitHubToolProvider::new();
    let temp_dir = TempDir::new().unwrap();
    let options = ToolOptions::new().with_cache_dir(temp_dir.path().to_path_buf());

    let resolved = ResolvedTool {
        name: "foundationdb".to_string(),
        version: "7.3.63".to_string(),
        platform: Platform::new(Os::Darwin, Arch::Arm64),
        source: ToolSource::GitHub {
            repo: "apple/foundationdb".to_string(),
            tag: "7.3.63".to_string(),
            asset: "FoundationDB-7.3.63_arm64.pkg".to_string(),
            extract: vec![ToolExtract::Lib {
                path: "libfdb_c.dylib".to_string(),
                env: None,
            }],
        },
    };

    let target = provider
        .cache_targets_from_source(&resolved, &options)
        .into_iter()
        .next()
        .unwrap();
    assert!(target.ends_with("github/foundationdb/7.3.63/lib/libfdb_c.dylib"));
}

#[test]
fn test_cache_targets_from_source_uses_bin_for_default_extract() {
    let provider = GitHubToolProvider::new();
    let temp_dir = TempDir::new().unwrap();
    let options = ToolOptions::new().with_cache_dir(temp_dir.path().to_path_buf());

    let resolved = ResolvedTool {
        name: "foundationdb".to_string(),
        version: "7.3.63".to_string(),
        platform: Platform::new(Os::Darwin, Arch::Arm64),
        source: ToolSource::GitHub {
            repo: "apple/foundationdb".to_string(),
            tag: "7.3.63".to_string(),
            asset: "FoundationDB-7.3.63_arm64.pkg".to_string(),
            extract: vec![],
        },
    };
    let target = provider
        .cache_targets_from_source(&resolved, &options)
        .into_iter()
        .next()
        .unwrap();
    assert!(target.ends_with("github/foundationdb/7.3.63/bin/foundationdb"));
}

// ==========================================================================
// get_effective_token tests
// ==========================================================================

#[test]
fn test_get_effective_token_runtime_only() {
    with_token_env(None, None, || {
        let token = GitHubToolProvider::get_effective_token(Some("runtime-token"));
        assert_eq!(token, Some("runtime-token".to_string()));
    });
}

#[test]
fn test_get_effective_token_none() {
    with_token_env(None, None, || {
        let token = GitHubToolProvider::get_effective_token(None);
        assert!(token.is_none());
    });
}

#[test]
fn test_get_effective_token_github_token_priority() {
    with_token_env(Some("github-token"), Some("gh-token"), || {
        let token = GitHubToolProvider::get_effective_token(Some("runtime-token"));
        assert_eq!(token, Some("github-token".to_string()));
    });
}

#[test]
fn test_get_effective_token_gh_token_fallback() {
    with_token_env(None, Some("gh-token"), || {
        let token = GitHubToolProvider::get_effective_token(Some("runtime-token"));
        assert_eq!(token, Some("gh-token".to_string()));
    });
}

// ==========================================================================
// RateLimitInfo tests
// ==========================================================================

#[test]
fn test_rate_limit_info_from_headers() {
    use reqwest::header::{HeaderMap, HeaderValue};

    let mut headers = HeaderMap::new();
    headers.insert("x-ratelimit-limit", HeaderValue::from_static("60"));
    headers.insert("x-ratelimit-remaining", HeaderValue::from_static("0"));
    headers.insert("x-ratelimit-reset", HeaderValue::from_static("1735689600"));

    let info = RateLimitInfo::from_headers(&headers);
    assert_eq!(info.limit, Some(60));
    assert_eq!(info.remaining, Some(0));
    assert_eq!(info.reset, Some(1_735_689_600));
    assert!(info.is_exceeded());
}

#[test]
fn test_rate_limit_info_not_exceeded() {
    use reqwest::header::{HeaderMap, HeaderValue};

    let mut headers = HeaderMap::new();
    headers.insert("x-ratelimit-limit", HeaderValue::from_static("5000"));
    headers.insert("x-ratelimit-remaining", HeaderValue::from_static("4999"));

    let info = RateLimitInfo::from_headers(&headers);
    assert!(!info.is_exceeded());
}

#[test]
fn test_rate_limit_info_format_status() {
    let info = RateLimitInfo {
        limit: Some(60),
        remaining: Some(0),
        reset: None,
    };
    assert_eq!(
        info.format_status(),
        Some("0/60 requests remaining".to_string())
    );

    let info_partial = RateLimitInfo {
        limit: Some(60),
        remaining: None,
        reset: None,
    };
    assert_eq!(info_partial.format_status(), None);
}

#[test]
fn test_rate_limit_info_empty_headers() {
    let headers = reqwest::header::HeaderMap::new();
    let info = RateLimitInfo::from_headers(&headers);

    assert_eq!(info.limit, None);
    assert_eq!(info.remaining, None);
    assert_eq!(info.reset, None);
    assert!(!info.is_exceeded());
}

#[test]
fn test_rate_limit_info_default() {
    let info = RateLimitInfo::default();
    assert_eq!(info.limit, None);
    assert_eq!(info.remaining, None);
    assert_eq!(info.reset, None);
    assert!(!info.is_exceeded());
}

#[test]
fn test_rate_limit_info_format_status_missing_remaining() {
    let info = RateLimitInfo {
        limit: Some(60),
        remaining: None,
        reset: None,
    };
    assert!(info.format_status().is_none());
}

#[test]
fn test_rate_limit_info_format_status_missing_limit() {
    let info = RateLimitInfo {
        limit: None,
        remaining: Some(50),
        reset: None,
    };
    assert!(info.format_status().is_none());
}

#[test]
fn test_rate_limit_info_format_reset_duration_none() {
    let info = RateLimitInfo {
        limit: None,
        remaining: None,
        reset: None,
    };
    assert!(info.format_reset_duration().is_none());
}

#[test]
fn test_rate_limit_info_format_reset_duration_past() {
    // Use a timestamp in the past
    let info = RateLimitInfo {
        limit: None,
        remaining: None,
        reset: Some(0), // epoch
    };
    // Should return "now" for past timestamps
    assert_eq!(info.format_reset_duration(), Some("now".to_string()));
}

#[test]
fn test_rate_limit_info_invalid_header_values() {
    use reqwest::header::{HeaderMap, HeaderValue};

    let mut headers = HeaderMap::new();
    headers.insert(
        "x-ratelimit-limit",
        HeaderValue::from_static("not-a-number"),
    );
    headers.insert("x-ratelimit-remaining", HeaderValue::from_static("invalid"));

    let info = RateLimitInfo::from_headers(&headers);
    assert_eq!(info.limit, None);
    assert_eq!(info.remaining, None);
}

// ==========================================================================
// build_api_error tests
// ==========================================================================

#[test]
fn test_build_api_error_rate_limit_exceeded_unauthenticated() {
    let rate_limit = RateLimitInfo {
        limit: Some(60),
        remaining: Some(0),
        reset: Some(1_735_689_600),
    };

    let error = GitHubToolProvider::build_api_error(
        reqwest::StatusCode::FORBIDDEN,
        &rate_limit,
        false,
        "release owner/repo v1.0.0",
    );

    let msg = error.to_string();
    assert!(msg.contains("rate limit exceeded"));
}

#[test]
fn test_build_api_error_rate_limit_exceeded_authenticated() {
    let rate_limit = RateLimitInfo {
        limit: Some(5000),
        remaining: Some(0),
        reset: None,
    };

    let error = GitHubToolProvider::build_api_error(
        reqwest::StatusCode::FORBIDDEN,
        &rate_limit,
        true,
        "release owner/repo v1.0.0",
    );

    let msg = error.to_string();
    assert!(msg.contains("rate limit exceeded"));
}

#[test]
fn test_build_api_error_forbidden_not_rate_limit() {
    let rate_limit = RateLimitInfo {
        limit: Some(60),
        remaining: Some(30),
        reset: None,
    };

    let error = GitHubToolProvider::build_api_error(
        reqwest::StatusCode::FORBIDDEN,
        &rate_limit,
        false,
        "release owner/repo v1.0.0",
    );

    let msg = error.to_string();
    assert!(msg.contains("Access denied"));
}

#[test]
fn test_build_api_error_not_found() {
    let rate_limit = RateLimitInfo::default();

    let error = GitHubToolProvider::build_api_error(
        reqwest::StatusCode::NOT_FOUND,
        &rate_limit,
        false,
        "release owner/repo v999.0.0",
    );

    let msg = error.to_string();
    assert!(msg.contains("not found"));
    assert!(msg.contains("404"));
}

#[test]
fn test_build_api_error_unauthorized() {
    let rate_limit = RateLimitInfo::default();

    let error = GitHubToolProvider::build_api_error(
        reqwest::StatusCode::UNAUTHORIZED,
        &rate_limit,
        true,
        "release owner/repo v1.0.0",
    );

    let msg = error.to_string();
    assert!(msg.contains("Authentication failed"));
    assert!(msg.contains("401"));
}

#[test]
fn test_build_api_error_server_error() {
    let rate_limit = RateLimitInfo::default();

    let error = GitHubToolProvider::build_api_error(
        reqwest::StatusCode::INTERNAL_SERVER_ERROR,
        &rate_limit,
        false,
        "asset download",
    );

    let msg = error.to_string();
    assert!(msg.contains("HTTP 500"));
}

// ==========================================================================
// is_cached tests
// ==========================================================================

#[test]
fn test_is_cached_not_cached() {
    let provider = GitHubToolProvider::new();
    let temp_dir = TempDir::new().unwrap();
    let options = ToolOptions::new().with_cache_dir(temp_dir.path().to_path_buf());

    let resolved = ResolvedTool {
        name: "mytool".to_string(),
        version: "1.0.0".to_string(),
        platform: Platform::new(Os::Darwin, Arch::Arm64),
        source: ToolSource::GitHub {
            repo: "owner/repo".to_string(),
            tag: "v1.0.0".to_string(),
            asset: "mytool.tar.gz".to_string(),
            extract: vec![],
        },
    };

    assert!(!provider.is_cached(&resolved, &options));
}

#[test]
fn test_is_cached_cached() {
    let provider = GitHubToolProvider::new();
    let temp_dir = TempDir::new().unwrap();
    let options = ToolOptions::new().with_cache_dir(temp_dir.path().to_path_buf());

    let resolved = ResolvedTool {
        name: "mytool".to_string(),
        version: "1.0.0".to_string(),
        platform: Platform::new(Os::Darwin, Arch::Arm64),
        source: ToolSource::GitHub {
            repo: "owner/repo".to_string(),
            tag: "v1.0.0".to_string(),
            asset: "mytool.tar.gz".to_string(),
            extract: vec![],
        },
    };

    // Create the cached file
    let cache_dir = provider.tool_cache_dir(&options, "mytool", "1.0.0");
    let bin_dir = cache_dir.join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    std::fs::write(bin_dir.join("mytool"), b"binary").unwrap();

    assert!(provider.is_cached(&resolved, &options));
}

#[test]
fn test_is_cached_library_path_uses_lib_directory() {
    let provider = GitHubToolProvider::new();
    let temp_dir = TempDir::new().unwrap();
    let options = ToolOptions::new().with_cache_dir(temp_dir.path().to_path_buf());

    let resolved = ResolvedTool {
        name: "foundationdb".to_string(),
        version: "7.3.63".to_string(),
        platform: Platform::new(Os::Darwin, Arch::Arm64),
        source: ToolSource::GitHub {
            repo: "apple/foundationdb".to_string(),
            tag: "7.3.63".to_string(),
            asset: "FoundationDB-7.3.63_arm64.pkg".to_string(),
            extract: vec![ToolExtract::Lib {
                path: "libfdb_c.dylib".to_string(),
                env: None,
            }],
        },
    };

    let cache_dir = provider.tool_cache_dir(&options, "foundationdb", "7.3.63");
    let lib_dir = cache_dir.join("lib");
    std::fs::create_dir_all(&lib_dir).unwrap();
    std::fs::write(lib_dir.join("libfdb_c.dylib"), b"library").unwrap();

    assert!(provider.is_cached(&resolved, &options));
}

// ==========================================================================
// Release and Asset struct tests
// ==========================================================================

#[test]
fn test_release_deserialization() {
    let json = r#"{
            "tag_name": "v1.0.0",
            "name": "release title",
            "assets": [
                {"name": "tool-linux.tar.gz", "browser_download_url": "https://example.com/linux.tar.gz"},
                {"name": "tool-darwin.tar.gz", "browser_download_url": "https://example.com/darwin.tar.gz"}
            ]
        }"#;

    let release: Release = serde_json::from_str(json).unwrap();
    assert_eq!(release.assets.len(), 2);
    assert_eq!(release.assets[0].name, "tool-linux.tar.gz");
    assert_eq!(
        release.assets[0].browser_download_url,
        "https://example.com/linux.tar.gz"
    );
}

#[test]
fn test_release_deserialization_empty_assets() {
    let json = r#"{"tag_name": "v0.1.0", "assets": []}"#;
    let release: Release = serde_json::from_str(json).unwrap();
    assert!(release.assets.is_empty());
}

// ==========================================================================
// tar.gz / tar.xz archive extraction tests
// ==========================================================================

fn build_tar_gz(entries: &[(&str, &[u8], u32)]) -> Vec<u8> {
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::io::Write;
    use tar::{Builder, Header};

    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    {
        let mut builder = Builder::new(&mut encoder);
        for (path, contents, mode) in entries {
            let mut header = Header::new_gnu();
            header.set_path(path).unwrap();
            header.set_size(contents.len() as u64);
            header.set_mode(*mode);
            header.set_cksum();
            builder.append(&header, *contents).unwrap();
        }
        builder.finish().unwrap();
    }
    encoder.flush().unwrap();
    encoder.finish().unwrap()
}

fn build_tar_xz(entries: &[(&str, &[u8], u32)]) -> Vec<u8> {
    use std::io::Write;
    use tar::{Builder, Header};

    let mut encoder = xz2::write::XzEncoder::new(Vec::new(), 6);
    {
        let mut builder = Builder::new(&mut encoder);
        for (path, contents, mode) in entries {
            let mut header = Header::new_gnu();
            header.set_path(path).unwrap();
            header.set_size(contents.len() as u64);
            header.set_mode(*mode);
            header.set_cksum();
            builder.append(&header, *contents).unwrap();
        }
        builder.finish().unwrap();
    }
    encoder.flush().unwrap();
    encoder.finish().unwrap()
}

#[test]
fn test_extract_from_tar_gz_specific_path() {
    let provider = GitHubToolProvider::new();
    let data = build_tar_gz(&[
        ("treefmt_linux_amd64/treefmt", b"#!/bin/sh\n", 0o755),
        ("treefmt_linux_amd64/README", b"readme\n", 0o644),
    ]);
    let temp = TempDir::new().unwrap();
    let dest = temp.path().join("treefmt");

    let extracted = provider
        .extract_from_tar_gz(&data, Some("treefmt"), &dest)
        .unwrap();

    assert_eq!(extracted, dest.join("treefmt"));
    assert!(extracted.exists());
}

#[test]
fn test_extract_from_tar_xz_specific_path() {
    let provider = GitHubToolProvider::new();
    let data = build_tar_xz(&[
        (
            "weaver-aarch64-apple-darwin/weaver",
            b"#!/bin/sh\necho weaver\n",
            0o755,
        ),
        ("weaver-aarch64-apple-darwin/README.md", b"readme\n", 0o644),
    ]);
    let temp = TempDir::new().unwrap();
    let dest = temp.path().join("weaver");

    let extracted = provider
        .extract_from_tar_xz(&data, Some("weaver-aarch64-apple-darwin/weaver"), &dest)
        .unwrap();

    assert_eq!(extracted, dest.join("weaver"));
    assert!(extracted.exists());
    let contents = std::fs::read(&extracted).unwrap();
    assert_eq!(contents, b"#!/bin/sh\necho weaver\n");
}

#[test]
fn test_extract_from_tar_xz_extracts_all() {
    // The github extractor doesn't flatten single-root prefixes (unlike URL).
    // Lay the archive out with bin/weaver at the top level so
    // `find_main_binary` can locate the primary executable.
    let provider = GitHubToolProvider::new();
    let data = build_tar_xz(&[
        ("bin/weaver", b"#!/bin/sh\necho weaver\n", 0o755),
        ("LICENSE", b"MIT\n", 0o644),
    ]);
    let temp = TempDir::new().unwrap();
    let dest = temp.path().join("weaver");

    let extracted = provider.extract_from_tar_xz(&data, None, &dest).unwrap();

    assert_eq!(extracted, dest.join("bin").join("weaver"));
    assert!(extracted.exists());
    assert!(dest.join("LICENSE").exists());
}

#[test]
fn test_extract_from_tar_gz_extracts_all() {
    // Parity with the tar.xz version: the github extractor doesn't flatten
    // single-root prefixes, so we lay the archive out with bin/treefmt at
    // the top level so `find_main_binary` can locate the primary executable.
    let provider = GitHubToolProvider::new();
    let data = build_tar_gz(&[
        ("bin/treefmt", b"#!/bin/sh\necho treefmt\n", 0o755),
        ("LICENSE", b"MIT\n", 0o644),
    ]);
    let temp = TempDir::new().unwrap();
    let dest = temp.path().join("treefmt");

    let extracted = provider.extract_from_tar_gz(&data, None, &dest).unwrap();

    assert_eq!(extracted, dest.join("bin").join("treefmt"));
    assert!(extracted.exists());
    assert!(dest.join("LICENSE").exists());
}

#[test]
fn test_extract_from_tar_matches_path_components_not_suffix() {
    // Regression: an entry like `notweaver/x` must NOT match the
    // requested binary path `weaver`. The match must be anchored on
    // a `/` boundary (or full-string equality), never a raw suffix.
    let provider = GitHubToolProvider::new();
    let data = build_tar_xz(&[
        ("notweaver/x", b"decoy\n", 0o755),
        ("subweaver", b"another-decoy\n", 0o755),
    ]);
    let temp = TempDir::new().unwrap();
    let dest = temp.path().join("weaver");

    let err = provider
        .extract_from_tar_xz(&data, Some("weaver"), &dest)
        .unwrap_err();
    assert!(
        err.to_string().contains("not found"),
        "expected 'not found' error, got: {}",
        err
    );
}

#[test]
fn test_extract_from_tar_xz_missing_binary_errors() {
    let provider = GitHubToolProvider::new();
    let data = build_tar_xz(&[("weaver-aarch64-apple-darwin/weaver", b"#!/bin/sh\n", 0o755)]);
    let temp = TempDir::new().unwrap();
    let dest = temp.path().join("weaver");

    let err = provider
        .extract_from_tar_xz(&data, Some("not/in/archive"), &dest)
        .unwrap_err();
    assert!(err.to_string().contains("not found"));
}

#[test]
fn test_extract_binary_detects_tar_xz_extension() {
    let provider = GitHubToolProvider::new();
    let data = build_tar_xz(&[(
        "weaver-aarch64-apple-darwin/weaver",
        b"#!/bin/sh\necho weaver\n",
        0o755,
    )]);
    let temp = TempDir::new().unwrap();
    let dest = temp.path().join("weaver");

    let extracted = provider
        .extract_binary(
            &data,
            "weaver-aarch64-apple-darwin.tar.xz",
            Some("weaver-aarch64-apple-darwin/weaver"),
            &dest,
        )
        .unwrap();

    assert_eq!(extracted, dest.join("weaver"));
    assert!(extracted.exists());
}

#[test]
fn test_extract_binary_detects_txz_extension() {
    let provider = GitHubToolProvider::new();
    let data = build_tar_xz(&[(
        "weaver-aarch64-apple-darwin/weaver",
        b"#!/bin/sh\necho weaver\n",
        0o755,
    )]);
    let temp = TempDir::new().unwrap();
    let dest = temp.path().join("weaver");

    let extracted = provider
        .extract_binary(
            &data,
            "weaver.txz",
            Some("weaver-aarch64-apple-darwin/weaver"),
            &dest,
        )
        .unwrap();

    assert_eq!(extracted, dest.join("weaver"));
    assert!(extracted.exists());
}
