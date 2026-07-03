use super::*;
use tempfile::TempDir;

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

fn build_zip(entries: &[(&str, &[u8], u32)]) -> Vec<u8> {
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    let mut buffer = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buffer);
        for (path, contents, mode) in entries {
            let options = SimpleFileOptions::default().unix_permissions(*mode);
            writer.start_file(*path, options).unwrap();
            writer.write_all(contents).unwrap();
        }
        writer.finish().unwrap();
    }
    buffer.into_inner()
}

fn temp_dir() -> TempDir {
    TempDir::new().unwrap()
}

#[test]
fn test_path_looks_like_library() {
    assert!(path_looks_like_library("libfoo.so"));
    assert!(path_looks_like_library("libfoo.so.6"));
    assert!(path_looks_like_library("Foo.dylib"));
    assert!(path_looks_like_library("foo.DLL"));
    assert!(!path_looks_like_library("bin/foo"));
    assert!(!path_looks_like_library("foo.sole"));
}

#[test]
fn test_suffix_checks_handle_non_ascii_names() {
    // Regression: the suffix check used to byte-slice the &str, which
    // panics when the boundary lands inside a multi-byte character
    // ("xéxx" with ".so" slices at byte 2, mid-'é').
    assert!(!path_looks_like_library("xéxx"));
    assert!(!path_looks_like_library("outil-é"));
    assert!(path_looks_like_library("libé.so"));

    let temp = temp_dir();
    let dest = temp.path().join("tool");
    let extracted = extract_binary(b"#!/bin/sh\n", "outil-\u{e9}x", None, &dest).unwrap();
    assert!(extracted.exists());
}

#[test]
fn test_extract_preserves_previous_dest_on_success() {
    // Re-extraction over an existing destination replaces it and leaves
    // no backup/temp siblings behind.
    let temp = temp_dir();
    let dest = temp.path().join("tool");

    let first = build_tar_gz(&[("tool-1.0/tool", b"#!/bin/sh\necho one\n", 0o755)]);
    extract_from_tar_gz(&first, None, &dest).unwrap();
    let second = build_tar_gz(&[("tool-2.0/tool", b"#!/bin/sh\necho two\n", 0o755)]);
    let extracted = extract_from_tar_gz(&second, None, &dest).unwrap();

    let contents = std::fs::read(&extracted).unwrap();
    assert_eq!(contents, b"#!/bin/sh\necho two\n");
    let siblings: Vec<String> = std::fs::read_dir(temp.path())
        .unwrap()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        siblings,
        vec!["tool".to_string()],
        "no leftover temp/backup dirs"
    );
}

#[test]
fn test_file_looks_like_library() {
    assert!(file_looks_like_library(Path::new("/x/libfoo.so")));
    assert!(file_looks_like_library(Path::new("/x/libfoo.so.6.1")));
    assert!(!file_looks_like_library(Path::new("/x/bin/foo")));
}

#[test]
fn test_extract_from_tar_gz_specific_path() {
    let data = build_tar_gz(&[
        ("treefmt_linux_amd64/treefmt", b"#!/bin/sh\n", 0o755),
        ("treefmt_linux_amd64/README", b"readme\n", 0o644),
    ]);
    let temp = temp_dir();
    let dest = temp.path().join("treefmt");

    let extracted = extract_from_tar_gz(&data, Some("treefmt"), &dest).unwrap();

    assert_eq!(extracted, dest.join("treefmt"));
    assert!(extracted.exists());
}

#[test]
fn test_extract_from_tar_xz_specific_path() {
    let data = build_tar_xz(&[
        (
            "weaver-aarch64-apple-darwin/weaver",
            b"#!/bin/sh\necho weaver\n",
            0o755,
        ),
        ("weaver-aarch64-apple-darwin/README.md", b"readme\n", 0o644),
    ]);
    let temp = temp_dir();
    let dest = temp.path().join("weaver");

    let extracted =
        extract_from_tar_xz(&data, Some("weaver-aarch64-apple-darwin/weaver"), &dest).unwrap();

    assert_eq!(extracted, dest.join("weaver"));
    assert!(extracted.exists());
    let contents = std::fs::read(&extracted).unwrap();
    assert_eq!(contents, b"#!/bin/sh\necho weaver\n");
}

#[test]
fn test_extract_from_tar_xz_extracts_all_and_flattens_prefix() {
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

    let extracted = extract_from_tar_xz(&data, None, &dest).unwrap();

    // The single-root directory is promoted and the preferred executable
    // surfaced as the primary binary.
    assert!(extracted.exists());
    assert!(dest.join("weaver").exists());
    assert!(dest.join("LICENSE").exists());
}

#[test]
fn test_extract_from_tar_gz_extracts_all_without_single_root() {
    let data = build_tar_gz(&[
        ("bin/treefmt", b"#!/bin/sh\necho treefmt\n", 0o755),
        ("LICENSE", b"MIT\n", 0o644),
    ]);
    let temp = temp_dir();
    let dest = temp.path().join("treefmt");

    let extracted = extract_from_tar_gz(&data, None, &dest).unwrap();

    assert_eq!(extracted, dest.join("bin").join("treefmt"));
    assert!(extracted.exists());
    assert!(dest.join("LICENSE").exists());
}

#[test]
fn test_extract_from_tar_gz_flattens_node24_prefix_layout() {
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

    let extracted = extract_from_tar_gz(&data, None, &dest).unwrap();

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
fn test_extract_from_tar_gz_prefers_tool_name_for_versioned_cache_dir() {
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
    ]);
    let temp = temp_dir();
    // Versioned cache layout: .../node/24.14.0
    let dest = temp.path().join("node").join("24.14.0");

    let extracted = extract_from_tar_gz(&data, None, &dest).unwrap();

    // The parent directory name ("node") is preferred over the first
    // alphabetical bin entry ("node" sorts before "npm" here anyway, but
    // the preference guards layouts where it does not).
    assert_eq!(extracted, dest.join("bin").join("node"));
}

#[test]
fn test_extract_from_tar_matches_path_components_not_suffix() {
    // Regression: an entry like `notweaver/x` must NOT match the requested
    // binary path `weaver`. The match must be anchored on a `/` boundary
    // (or full-string equality), never a raw suffix.
    let data = build_tar_xz(&[
        ("notweaver/x", b"decoy\n", 0o755),
        ("subweaver", b"another-decoy\n", 0o755),
    ]);
    let temp = temp_dir();
    let dest = temp.path().join("weaver");

    let err = extract_from_tar_xz(&data, Some("weaver"), &dest).unwrap_err();
    assert!(
        err.to_string().contains("not found"),
        "expected 'not found' error, got: {err}"
    );
}

#[test]
fn test_extract_from_tar_xz_missing_binary_errors() {
    let data = build_tar_xz(&[("weaver-aarch64-apple-darwin/weaver", b"#!/bin/sh\n", 0o755)]);
    let temp = temp_dir();
    let dest = temp.path().join("weaver");

    let err = extract_from_tar_xz(&data, Some("not/in/archive"), &dest).unwrap_err();
    assert!(err.to_string().contains("not found"));
}

#[test]
fn test_extract_binary_detects_tar_xz_extension() {
    let data = build_tar_xz(&[(
        "weaver-aarch64-apple-darwin/weaver",
        b"#!/bin/sh\necho weaver\n",
        0o755,
    )]);
    let temp = temp_dir();
    let dest = temp.path().join("weaver");

    let extracted = extract_binary(
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
    let data = build_tar_xz(&[(
        "weaver-aarch64-apple-darwin/weaver",
        b"#!/bin/sh\necho weaver\n",
        0o755,
    )]);
    let temp = temp_dir();
    let dest = temp.path().join("weaver");

    let extracted = extract_binary(
        &data,
        "weaver.txz",
        Some("weaver-aarch64-apple-darwin/weaver"),
        &dest,
    )
    .unwrap();

    assert_eq!(extracted, dest.join("weaver"));
    assert!(extracted.exists());
}

#[test]
fn test_extract_binary_ignores_url_query_string() {
    let data = build_tar_gz(&[("tool-1.0/tool", b"#!/bin/sh\n", 0o755)]);
    let temp = temp_dir();
    let dest = temp.path().join("tool");

    let extracted = extract_binary(
        &data,
        "https://example.com/tool.tar.gz?token=abc",
        Some("tool-1.0/tool"),
        &dest,
    )
    .unwrap();

    assert_eq!(extracted, dest.join("tool"));
}

#[test]
fn test_extract_binary_raw_binary_fallback() {
    let temp = temp_dir();
    let dest = temp.path().join("mytool");

    let extracted =
        extract_binary(b"#!/bin/sh\necho hi\n", "mytool-linux-amd64", None, &dest).unwrap();

    assert_eq!(extracted, dest.join("mytool-linux-amd64"));
    assert!(extracted.exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&extracted).unwrap().permissions().mode();
        assert_eq!(mode & 0o111, 0o111);
    }
}

#[test]
fn test_extract_from_zip_specific_path() {
    let data = build_zip(&[
        ("tool-1.0/tool", b"#!/bin/sh\necho tool\n", 0o755),
        ("tool-1.0/README", b"readme\n", 0o644),
    ]);
    let temp = temp_dir();
    let dest = temp.path().join("tool");

    let extracted = extract_from_zip(&data, Some("tool"), &dest).unwrap();

    assert_eq!(extracted, dest.join("tool"));
    assert!(extracted.exists());
}

#[test]
fn test_extract_from_zip_extracts_all_and_flattens_prefix() {
    let data = build_zip(&[
        ("tool-1.0/bin/tool", b"#!/bin/sh\necho tool\n", 0o755),
        ("tool-1.0/LICENSE", b"MIT\n", 0o644),
    ]);
    let temp = temp_dir();
    let dest = temp.path().join("tool");

    let extracted = extract_from_zip(&data, None, &dest).unwrap();

    assert_eq!(extracted, dest.join("bin").join("tool"));
    assert!(dest.join("LICENSE").exists());
}

#[test]
fn test_extract_from_zip_missing_binary_errors() {
    let data = build_zip(&[("tool-1.0/tool", b"#!/bin/sh\n", 0o755)]);
    let temp = temp_dir();
    let dest = temp.path().join("tool");

    let err = extract_from_zip(&data, Some("missing"), &dest).unwrap_err();
    assert!(err.to_string().contains("not found"));
}

#[test]
fn test_looks_like_prefix_install() {
    let temp = temp_dir();
    assert!(!looks_like_prefix_install(temp.path()));
    std::fs::create_dir_all(temp.path().join("bin")).unwrap();
    assert!(looks_like_prefix_install(temp.path()));
}

#[test]
fn test_find_primary_binary_in_prefix_prefers_tool_name() {
    let temp = temp_dir();
    let bin = temp.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join("aaa"), b"#!/bin/sh\n").unwrap();
    std::fs::write(bin.join("mytool"), b"#!/bin/sh\n").unwrap();

    let found = find_primary_binary_in_prefix(temp.path(), "mytool").unwrap();
    assert_eq!(found, bin.join("mytool"));
}

#[cfg(not(target_os = "macos"))]
#[test]
fn test_extract_from_pkg_unsupported_off_macos() {
    let temp = temp_dir();
    let err = extract_from_pkg(b"pkg", None, temp.path()).unwrap_err();
    assert!(err.to_string().contains("only supported on macOS"));
}

// ==========================================================================
// pkg payload entry-path validation (platform-neutral logic for the
// macOS-only cpio flow)
// ==========================================================================

mod entry_paths {
    use crate::entry_paths::{find_unsafe_entry, is_safe_entry};

    #[test]
    fn safe_entries() {
        for entry in [
            ".",
            "./",
            "./usr/local/bin/tool",
            "usr/local/bin/tool",
            "deeply/nested/dir/file.txt",
            "./name-with..dots/file",
            "..leading-dots-name",
        ] {
            assert!(is_safe_entry(entry), "expected safe: {entry:?}");
        }
    }

    #[test]
    fn unsafe_entries() {
        for entry in [
            "",
            "../x",
            "..",
            "./../x",
            "/absolute/path",
            "/",
            "a/../../b",
            "usr/../../../etc/passwd",
        ] {
            assert!(!is_safe_entry(entry), "expected unsafe: {entry:?}");
        }
    }

    #[test]
    fn find_unsafe_entry_skips_blank_lines() {
        let listing = ".\n./usr/bin/tool\n\n   \n./usr/share/doc\n";
        assert_eq!(find_unsafe_entry(listing.lines()), None);
    }

    #[test]
    fn find_unsafe_entry_reports_first_offender() {
        let listing = "./usr/bin/tool\n../escape\n/abs/path\n";
        assert_eq!(find_unsafe_entry(listing.lines()), Some("../escape"));
    }

    #[test]
    fn find_unsafe_entry_trims_whitespace() {
        let listing = "  ./ok  \n\t../bad\n";
        assert_eq!(find_unsafe_entry(listing.lines()), Some("../bad"));
    }
}
