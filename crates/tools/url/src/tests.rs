use super::*;
use cuenv_core::tools::{Arch, Os, Platform};
use flate2::Compression;
use flate2::write::GzEncoder;
use std::io::Write;
use tar::{Builder, Header};
use tempfile::TempDir;

fn build_tar_gz(entries: &[(&str, &[u8], u32)]) -> Vec<u8> {
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

fn temp_dir() -> TempDir {
    tempfile::Builder::new()
        .prefix("cuenv_url_provider_")
        .tempdir()
        .unwrap()
}

#[test]
fn test_expand_template_version() {
    let result = UrlToolProvider::expand_template(
        "https://example.com/tool-{version}.tar.gz",
        "1.2.3",
        &Platform::new(Os::Linux, Arch::X86_64),
    );
    assert_eq!(result, "https://example.com/tool-1.2.3.tar.gz");
}

#[test]
fn test_expand_template_os_linux() {
    let result = UrlToolProvider::expand_template(
        "https://example.com/tool-{os}.tar.gz",
        "1.0.0",
        &Platform::new(Os::Linux, Arch::X86_64),
    );
    assert_eq!(result, "https://example.com/tool-linux.tar.gz");
}

#[test]
fn test_expand_template_os_darwin() {
    let result = UrlToolProvider::expand_template(
        "https://example.com/tool-{os}.tar.gz",
        "1.0.0",
        &Platform::new(Os::Darwin, Arch::Arm64),
    );
    assert_eq!(result, "https://example.com/tool-darwin.tar.gz");
}

#[test]
fn test_expand_template_arch_x86_64() {
    let result = UrlToolProvider::expand_template(
        "https://example.com/tool-{arch}.tar.gz",
        "1.0.0",
        &Platform::new(Os::Linux, Arch::X86_64),
    );
    assert_eq!(result, "https://example.com/tool-x86_64.tar.gz");
}

#[test]
fn test_expand_template_arch_arm64() {
    let result = UrlToolProvider::expand_template(
        "https://example.com/tool-{arch}.tar.gz",
        "1.0.0",
        &Platform::new(Os::Linux, Arch::Arm64),
    );
    assert_eq!(result, "https://example.com/tool-aarch64.tar.gz");
}

#[test]
fn test_expand_template_all() {
    let result = UrlToolProvider::expand_template(
        "https://example.com/{version}/{os}/{arch}/tool.tar.gz",
        "2.0.0",
        &Platform::new(Os::Darwin, Arch::Arm64),
    );
    assert_eq!(
        result,
        "https://example.com/2.0.0/darwin/aarch64/tool.tar.gz"
    );
}

#[test]
fn test_path_looks_like_library() {
    assert!(UrlToolProvider::path_looks_like_library("libfoo.so"));
    assert!(UrlToolProvider::path_looks_like_library("libfoo.dylib"));
    assert!(UrlToolProvider::path_looks_like_library("foo.dll"));
    assert!(UrlToolProvider::path_looks_like_library("libfoo.so.1"));
    assert!(!UrlToolProvider::path_looks_like_library("foo"));
    assert!(!UrlToolProvider::path_looks_like_library("foo.tar.gz"));
}

#[test]
fn test_provider_name() {
    let provider = UrlToolProvider::new();
    assert_eq!(provider.name(), "url");
}

#[test]
fn test_provider_new_defers_client_initialization() {
    let provider = UrlToolProvider::new();
    assert!(provider.client.get().is_none());
}

#[test]
fn test_can_handle_url_source() {
    let provider = UrlToolProvider::new();
    let source = ToolSource::Url {
        url: "https://example.com/tool".to_string(),
        extract: vec![],
    };
    assert!(provider.can_handle(&source));
}

#[test]
fn test_cannot_handle_github_source() {
    let provider = UrlToolProvider::new();
    let source = ToolSource::GitHub {
        repo: "owner/repo".to_string(),
        tag: "v1.0.0".to_string(),
        asset: "tool.tar.gz".to_string(),
        extract: vec![],
    };
    assert!(!provider.can_handle(&source));
}

#[test]
fn test_extract_from_tar_gz_flattens_node24_prefix_layout() {
    let provider = UrlToolProvider::new();
    let data = build_tar_gz(&[
        (
            "node-v24.14.0-linux-x64/bin/node",
            b"#!/bin/sh\necho node24\n",
            0o755,
        ),
        (
            "node-v24.14.0-linux-x64/bin/npm",
            b"#!/bin/sh\necho npm24\n",
            0o755,
        ),
        (
            "node-v24.14.0-linux-x64/bin/npx",
            b"#!/bin/sh\necho npx24\n",
            0o755,
        ),
        (
            "node-v24.14.0-linux-x64/bin/corepack",
            b"#!/bin/sh\necho corepack24\n",
            0o755,
        ),
        (
            "node-v24.14.0-linux-x64/lib/node_modules/npm/package.json",
            br#"{"name":"npm"}"#,
            0o644,
        ),
        (
            "node-v24.14.0-linux-x64/include/node/node.h",
            b"#define NODE_MAJOR_VERSION 24\n",
            0o644,
        ),
    ]);
    let temp = temp_dir();
    let dest = temp.path().join("node");

    let extracted = provider.extract_from_tar_gz(&data, None, &dest).unwrap();

    assert_eq!(extracted, dest.join("bin").join("node"));
    assert!(dest.join("bin").join("npm").exists());
    assert!(dest.join("bin").join("npx").exists());
    assert!(dest.join("bin").join("corepack").exists());
    assert!(
        dest.join("lib")
            .join("node_modules")
            .join("npm")
            .join("package.json")
            .exists()
    );
    assert!(dest.join("include").join("node").join("node.h").exists());
}

#[test]
fn test_extract_from_tar_gz_preserves_node25_without_corepack() {
    let provider = UrlToolProvider::new();
    let data = build_tar_gz(&[
        (
            "node-v25.8.1-linux-x64/bin/node",
            b"#!/bin/sh\necho node25\n",
            0o755,
        ),
        (
            "node-v25.8.1-linux-x64/bin/npm",
            b"#!/bin/sh\necho npm25\n",
            0o755,
        ),
        (
            "node-v25.8.1-linux-x64/bin/npx",
            b"#!/bin/sh\necho npx25\n",
            0o755,
        ),
        (
            "node-v25.8.1-linux-x64/lib/node_modules/npm/package.json",
            br#"{"name":"npm"}"#,
            0o644,
        ),
    ]);
    let temp = temp_dir();
    let dest = temp.path().join("node");

    let extracted = provider.extract_from_tar_gz(&data, None, &dest).unwrap();

    assert_eq!(extracted, dest.join("bin").join("node"));
    assert!(dest.join("bin").join("node").exists());
    assert!(dest.join("bin").join("npm").exists());
    assert!(dest.join("bin").join("npx").exists());
    assert!(!dest.join("bin").join("corepack").exists());
}

#[test]
fn test_extract_from_tar_gz_prefers_tool_name_for_versioned_cache_dir() {
    let provider = UrlToolProvider::new();
    let data = build_tar_gz(&[
        (
            "node-v24.14.0-linux-x64/bin/node",
            b"#!/bin/sh\necho node24\n",
            0o755,
        ),
        (
            "node-v24.14.0-linux-x64/bin/npm",
            b"#!/bin/sh\necho npm24\n",
            0o755,
        ),
        (
            "node-v24.14.0-linux-x64/bin/npx",
            b"#!/bin/sh\necho npx24\n",
            0o755,
        ),
    ]);
    let temp = temp_dir();
    let dest = temp.path().join("node").join("24.14.0");

    let extracted = provider.extract_from_tar_gz(&data, None, &dest).unwrap();

    assert_eq!(extracted, dest.join("bin").join("node"));
}

#[tokio::test]
async fn test_resolve_simple_url() {
    let provider = UrlToolProvider::new();
    let config = serde_json::json!({
        "type": "url",
        "url": "https://example.com/tool-{version}-{os}-{arch}.tar.gz"
    });
    let platform = Platform::new(Os::Linux, Arch::X86_64);
    let request = ToolResolveRequest {
        tool_name: "mytool",
        version: "1.0.0",
        platform: &platform,
        config: &config,
        token: None,
    };

    let resolved = provider.resolve(&request).await.unwrap();
    assert_eq!(resolved.name, "mytool");
    assert_eq!(resolved.version, "1.0.0");

    match &resolved.source {
        ToolSource::Url { url, extract } => {
            assert_eq!(url, "https://example.com/tool-1.0.0-linux-x86_64.tar.gz");
            assert!(extract.is_empty());
        }
        _ => panic!("Expected URL source"),
    }
}

#[tokio::test]
async fn test_resolve_url_with_path() {
    let provider = UrlToolProvider::new();
    let config = serde_json::json!({
        "type": "url",
        "url": "https://example.com/tool-{version}.tar.gz",
        "path": "tool-{version}/bin/tool"
    });
    let platform = Platform::new(Os::Linux, Arch::X86_64);
    let request = ToolResolveRequest {
        tool_name: "mytool",
        version: "2.0.0",
        platform: &platform,
        config: &config,
        token: None,
    };

    let resolved = provider.resolve(&request).await.unwrap();
    match &resolved.source {
        ToolSource::Url { url, extract } => {
            assert_eq!(url, "https://example.com/tool-2.0.0.tar.gz");
            assert_eq!(extract.len(), 1);
            match &extract[0] {
                ToolExtract::Bin { path, .. } => {
                    assert_eq!(path, "tool-2.0.0/bin/tool");
                }
                _ => panic!("Expected Bin extract"),
            }
        }
        _ => panic!("Expected URL source"),
    }
}

#[test]
fn test_extract_from_tar_xz_specific_path() {
    let provider = UrlToolProvider::new();
    let data = build_tar_xz(&[
        (
            "weaver-aarch64-apple-darwin/weaver",
            b"#!/bin/sh\necho weaver\n",
            0o755,
        ),
        (
            "weaver-aarch64-apple-darwin/README.md",
            b"weaver readme\n",
            0o644,
        ),
    ]);
    let temp = temp_dir();
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
fn test_extract_from_tar_xz_extracts_all_and_flattens_prefix() {
    let provider = UrlToolProvider::new();
    let data = build_tar_xz(&[
        (
            "weaver-x86_64-unknown-linux-gnu/weaver",
            b"#!/bin/sh\necho weaver\n",
            0o755,
        ),
        ("weaver-x86_64-unknown-linux-gnu/LICENSE", b"MIT\n", 0o644),
    ]);
    let temp = temp_dir();
    let dest = temp.path().join("weaver");

    let extracted = provider.extract_from_tar_xz(&data, None, &dest).unwrap();

    // The provider promotes the single-root directory and surfaces the
    // first executable as the primary binary.
    assert!(extracted.exists());
    assert!(dest.join("weaver").exists());
    assert!(dest.join("LICENSE").exists());
}

#[test]
fn test_extract_from_tar_xz_missing_binary_returns_error() {
    let provider = UrlToolProvider::new();
    let data = build_tar_xz(&[(
        "weaver-aarch64-apple-darwin/weaver",
        b"#!/bin/sh\necho weaver\n",
        0o755,
    )]);
    let temp = temp_dir();
    let dest = temp.path().join("weaver");

    let err = provider
        .extract_from_tar_xz(&data, Some("not/in/archive"), &dest)
        .unwrap_err();
    assert!(err.to_string().contains("not found"));
}

#[test]
fn test_extract_binary_detects_tar_xz_extension() {
    let provider = UrlToolProvider::new();
    let data = build_tar_xz(&[(
        "weaver-aarch64-apple-darwin/weaver",
        b"#!/bin/sh\necho weaver\n",
        0o755,
    )]);
    let temp = temp_dir();
    let dest = temp.path().join("weaver");

    let extracted = provider
        .extract_binary(
            &data,
            "https://example.com/weaver-aarch64-apple-darwin.tar.xz",
            Some("weaver-aarch64-apple-darwin/weaver"),
            &dest,
        )
        .unwrap();

    assert_eq!(extracted, dest.join("weaver"));
    assert!(extracted.exists());
}

#[test]
fn test_extract_binary_detects_txz_extension() {
    let provider = UrlToolProvider::new();
    let data = build_tar_xz(&[(
        "weaver-aarch64-apple-darwin/weaver",
        b"#!/bin/sh\necho weaver\n",
        0o755,
    )]);
    let temp = temp_dir();
    let dest = temp.path().join("weaver");

    let extracted = provider
        .extract_binary(
            &data,
            "https://example.com/weaver.txz",
            Some("weaver-aarch64-apple-darwin/weaver"),
            &dest,
        )
        .unwrap();

    assert_eq!(extracted, dest.join("weaver"));
    assert!(extracted.exists());
}

#[tokio::test]
async fn test_resolve_url_missing_url_field() {
    let provider = UrlToolProvider::new();
    let config = serde_json::json!({
        "type": "url"
    });
    let platform = Platform::new(Os::Linux, Arch::X86_64);
    let request = ToolResolveRequest {
        tool_name: "mytool",
        version: "1.0.0",
        platform: &platform,
        config: &config,
        token: None,
    };

    assert!(provider.resolve(&request).await.is_err());
}
