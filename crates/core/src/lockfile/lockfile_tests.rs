use super::*;

#[test]
fn test_lockfile_serialization() {
    let mut lockfile = Lockfile::new();

    lockfile
        .upsert_runtime(
            ".".to_string(),
            LockedRuntime::Nix(LockedNixRuntime {
                flake: ".".to_string(),
                output: None,
                digest: "sha256:runtime123".to_string(),
                lockfile: "flake.lock".to_string(),
            }),
        )
        .unwrap();

    // OCI image artifact
    lockfile.artifacts.push(LockedArtifact {
        kind: ArtifactKind::Image {
            image: "nginx:1.25-alpine".to_string(),
            extract: vec![],
        },
        platforms: BTreeMap::from([
            (
                "darwin-arm64".to_string(),
                PlatformData {
                    digest: "sha256:abc123".to_string(),
                    size: Some(1234567),
                },
            ),
            (
                "linux-x86_64".to_string(),
                PlatformData {
                    digest: "sha256:def456".to_string(),
                    size: Some(1345678),
                },
            ),
        ]),
    });

    let toml_str = toml::to_string_pretty(&lockfile).unwrap();
    assert!(toml_str.contains("version = 4"));
    assert!(toml_str.contains("type = \"nix\""));
    assert!(toml_str.contains("lockfile = \"flake.lock\""));
    assert!(toml_str.contains("kind = \"image\""));
    assert!(toml_str.contains("nginx:1.25-alpine"));

    // Round-trip test
    let parsed: Lockfile = toml::from_str(&toml_str).unwrap();
    assert_eq!(parsed, lockfile);
}

#[test]
fn test_image_artifact_with_extract_roundtrip() {
    let mut lockfile = Lockfile::new();
    lockfile.artifacts.push(LockedArtifact {
        kind: ArtifactKind::Image {
            image: "nginx:1.25-alpine".to_string(),
            extract: vec![
                LockedOciExtract {
                    path: "/usr/sbin/nginx".to_string(),
                    as_name: None,
                },
                LockedOciExtract {
                    path: "/bin/sh".to_string(),
                    as_name: Some("busybox-sh".to_string()),
                },
            ],
        },
        platforms: BTreeMap::from([(
            "linux-x86_64".to_string(),
            PlatformData {
                digest: "sha256:def456".to_string(),
                size: Some(1234),
            },
        )]),
    });

    let toml_str = toml::to_string_pretty(&lockfile).unwrap();
    assert!(toml_str.contains("/usr/sbin/nginx"));
    assert!(toml_str.contains("busybox-sh"));

    let parsed: Lockfile = toml::from_str(&toml_str).unwrap();
    assert_eq!(parsed, lockfile);

    let ArtifactKind::Image { extract, .. } = &parsed.artifacts[0].kind;
    assert_eq!(extract.len(), 2);
    assert_eq!(extract[0].binary_name(), "nginx");
    assert_eq!(extract[1].binary_name(), "busybox-sh");
}

#[test]
fn test_legacy_image_artifact_without_extract_loads() {
    // Lockfiles written before #OCIExtract was propagated must keep
    // loading; activation will skip them with a warning rather than
    // silently extracting nothing.
    let legacy = r#"
version = 4

[[artifacts]]
kind = "image"
image = "nginx:1.25-alpine"

  [artifacts.platforms]
  "linux-x86_64" = { digest = "sha256:abc", size = 1234 }
"#;
    let parsed: Lockfile = toml::from_str(legacy).expect("legacy lockfile parses");
    let artifact = parsed
        .find_image_artifact("nginx:1.25-alpine")
        .expect("artifact present");
    let ArtifactKind::Image { extract, .. } = &artifact.kind;
    assert!(
        extract.is_empty(),
        "legacy artifact should default to an empty extract list"
    );
}

#[test]
fn test_locked_oci_extract_binary_name() {
    assert_eq!(
        LockedOciExtract {
            path: "/usr/sbin/nginx".to_string(),
            as_name: None,
        }
        .binary_name(),
        "nginx"
    );
    assert_eq!(
        LockedOciExtract {
            path: "/usr/sbin/nginx".to_string(),
            as_name: Some("my-nginx".to_string()),
        }
        .binary_name(),
        "my-nginx"
    );
    // Trailing slash should not produce an empty name.
    assert_eq!(
        LockedOciExtract {
            path: "/bin/".to_string(),
            as_name: None,
        }
        .binary_name(),
        "bin"
    );
    // Empty `as` falls back to path basename.
    assert_eq!(
        LockedOciExtract {
            path: "/bin/sh".to_string(),
            as_name: Some(String::new()),
        }
        .binary_name(),
        "sh"
    );
}

#[test]
fn test_find_image_artifact() {
    let mut lockfile = Lockfile::new();
    lockfile.artifacts.push(LockedArtifact {
        kind: ArtifactKind::Image {
            image: "nginx:1.25-alpine".to_string(),
            extract: vec![],
        },
        platforms: BTreeMap::new(),
    });

    assert!(lockfile.find_image_artifact("nginx:1.25-alpine").is_some());
    assert!(lockfile.find_image_artifact("nginx:1.24-alpine").is_none());
}

#[test]
fn test_upsert_artifact() {
    let mut lockfile = Lockfile::new();

    let artifact1 = LockedArtifact {
        kind: ArtifactKind::Image {
            image: "nginx:1.25-alpine".to_string(),
            extract: vec![],
        },
        platforms: BTreeMap::from([(
            "darwin-arm64".to_string(),
            PlatformData {
                digest: "sha256:old".to_string(),
                size: None,
            },
        )]),
    };

    lockfile.upsert_artifact(artifact1).unwrap();
    assert_eq!(lockfile.artifacts.len(), 1);

    // Update with new digest
    let artifact2 = LockedArtifact {
        kind: ArtifactKind::Image {
            image: "nginx:1.25-alpine".to_string(),
            extract: vec![],
        },
        platforms: BTreeMap::from([(
            "darwin-arm64".to_string(),
            PlatformData {
                digest: "sha256:new".to_string(),
                size: Some(123),
            },
        )]),
    };

    lockfile.upsert_artifact(artifact2).unwrap();
    assert_eq!(lockfile.artifacts.len(), 1);
    assert_eq!(
        lockfile.artifacts[0].platforms["darwin-arm64"].digest,
        "sha256:new"
    );
}

#[test]
fn test_current_platform() {
    let platform = current_platform();
    // Should contain OS and arch
    assert!(platform.contains('-'));
    let parts: Vec<&str> = platform.split('-').collect();
    assert_eq!(parts.len(), 2);
}

#[test]
fn test_normalize_platform() {
    assert_eq!(normalize_platform("macos-amd64"), "darwin-x86_64");
    assert_eq!(normalize_platform("linux-aarch64"), "linux-arm64");
    assert_eq!(normalize_platform("Darwin-ARM64"), "darwin-arm64");
}

#[test]
fn test_upsert_artifact_validation_empty_platforms() {
    let mut lockfile = Lockfile::new();

    let artifact = LockedArtifact {
        kind: ArtifactKind::Image {
            image: "nginx:1.25-alpine".to_string(),
            extract: vec![],
        },
        platforms: BTreeMap::new(), // Empty - should fail
    };

    let result = lockfile.upsert_artifact(artifact);
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("at least one platform")
    );
}

#[test]
fn test_upsert_artifact_validation_invalid_digest() {
    let mut lockfile = Lockfile::new();

    let artifact = LockedArtifact {
        kind: ArtifactKind::Image {
            image: "nginx:1.25-alpine".to_string(),
            extract: vec![],
        },
        platforms: BTreeMap::from([(
            "darwin-arm64".to_string(),
            PlatformData {
                digest: "invalid-no-prefix".to_string(), // Missing sha256: prefix
                size: None,
            },
        )]),
    };

    let result = lockfile.upsert_artifact(artifact);
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("Invalid digest format")
    );
}

#[test]
fn test_artifact_validate_valid() {
    let artifact = LockedArtifact {
        kind: ArtifactKind::Image {
            image: "nginx:1.25-alpine".to_string(),
            extract: vec![],
        },
        platforms: BTreeMap::from([
            (
                "darwin-arm64".to_string(),
                PlatformData {
                    digest: "sha256:abc123".to_string(),
                    size: Some(1234),
                },
            ),
            (
                "linux-x86_64".to_string(),
                PlatformData {
                    digest: "sha512:def456".to_string(),
                    size: None,
                },
            ),
        ]),
    };

    assert!(artifact.validate().is_ok());
}

#[test]
fn test_tools_serialization() {
    let mut lockfile = Lockfile::new();

    lockfile
            .upsert_tool_platform(
                "jq",
                "1.7.1",
                "darwin-arm64",
                LockedToolPlatform {
                    provider: "github".to_string(),
                    digest: "sha256:abc123".to_string(),
                    source: serde_json::json!({ "repo": "jqlang/jq", "tag": "jq-1.7.1", "asset": "jq-macos-arm64" }),
                    size: Some(1234567),
                    dependencies: vec![],
                },
            )
            .unwrap();

    lockfile
            .upsert_tool_platform(
                "jq",
                "1.7.1",
                "linux-x86_64",
                LockedToolPlatform {
                    provider: "github".to_string(),
                    digest: "sha256:def456".to_string(),
                    source: serde_json::json!({ "repo": "jqlang/jq", "tag": "jq-1.7.1", "asset": "jq-linux-amd64" }),
                    size: Some(1345678),
                    dependencies: vec![],
                },
            )
            .unwrap();

    let toml_str = toml::to_string_pretty(&lockfile).unwrap();
    assert!(toml_str.contains("version = 4"));
    assert!(toml_str.contains("[tools.jq]"));
    assert!(toml_str.contains("provider = \"github\""));
    assert!(toml_str.contains("digest = \"sha256:abc123\""));

    // Round-trip test
    let parsed: Lockfile = toml::from_str(&toml_str).unwrap();
    assert_eq!(parsed.tools.len(), 1);
    assert_eq!(parsed.tools["jq"].version, "1.7.1");
    assert_eq!(parsed.tools["jq"].platforms.len(), 2);
}

#[test]
fn test_tools_activation_serialization() {
    use crate::tools::{ToolActivationOperation, ToolActivationSource, ToolActivationStep};

    let mut lockfile = Lockfile::new();
    lockfile.tools_activation.push(ToolActivationStep {
        var: "PATH".to_string(),
        op: ToolActivationOperation::Prepend,
        separator: ":".to_string(),
        from: ToolActivationSource::AllBinDirs,
    });

    let toml_str = toml::to_string_pretty(&lockfile).unwrap();
    assert!(toml_str.contains("[[tools_activation]]"));
    assert!(toml_str.contains("var = \"PATH\""));
    assert!(toml_str.contains("op = \"prepend\""));
    assert!(toml_str.contains("type = \"allBinDirs\""));

    let parsed: Lockfile = toml::from_str(&toml_str).unwrap();
    assert_eq!(parsed.tools_activation.len(), 1);
    assert_eq!(parsed.tools_activation[0], lockfile.tools_activation[0]);
}

#[test]
fn test_find_runtime() {
    let mut lockfile = Lockfile::new();
    lockfile
        .upsert_runtime(
            ".".to_string(),
            LockedRuntime::Nix(LockedNixRuntime {
                flake: ".".to_string(),
                output: Some("devShells.x86_64-linux.default".to_string()),
                digest: "sha256:abc123".to_string(),
                lockfile: "flake.lock".to_string(),
            }),
        )
        .unwrap();

    assert!(lockfile.find_runtime(".").is_some());
    assert!(lockfile.find_runtime("apps/api").is_none());
}

#[test]
fn test_vcs_serialization() {
    let mut lockfile = Lockfile::new();
    lockfile
        .upsert_vcs(
            "mylib".to_string(),
            LockedVcsDependency {
                url: "https://github.com/example/mylib.git".to_string(),
                reference: "main".to_string(),
                commit: "0123456789abcdef0123456789abcdef01234567".to_string(),
                tree: "89abcdef012345670123456789abcdef01234567".to_string(),
                vendor: true,
                path: "vendor/mylib".to_string(),
                subdir: None,
                subtree: None,
                overlay: false,
                children: BTreeMap::new(),
            },
        )
        .unwrap();

    let toml_str = toml::to_string_pretty(&lockfile).unwrap();
    assert!(toml_str.contains("version = 4"));
    assert!(toml_str.contains("[vcs.mylib]"));
    assert!(toml_str.contains("reference = \"main\""));

    let parsed: Lockfile = toml::from_str(&toml_str).unwrap();
    assert_eq!(parsed.find_vcs("mylib").unwrap().path, "vendor/mylib");
}

#[test]
fn test_vcs_subdir_serialization_roundtrip() {
    let mut lockfile = Lockfile::new();
    lockfile
        .upsert_vcs(
            "skills".to_string(),
            LockedVcsDependency {
                url: "https://github.com/cuenv/cuenv.git".to_string(),
                reference: "0.27.1".to_string(),
                commit: "0123456789abcdef0123456789abcdef01234567".to_string(),
                tree: "89abcdef012345670123456789abcdef01234567".to_string(),
                vendor: true,
                path: ".agents/skills".to_string(),
                subdir: Some(".agents/skills".to_string()),
                subtree: Some("ffffffffffffffffffffffffffffffffffffffff".to_string()),
                overlay: false,
                children: BTreeMap::new(),
            },
        )
        .unwrap();

    let toml_str = toml::to_string_pretty(&lockfile).unwrap();
    assert!(toml_str.contains("subdir = \".agents/skills\""));
    assert!(toml_str.contains("subtree = \"ffffffffffffffffffffffffffffffffffffffff\""));

    let parsed: Lockfile = toml::from_str(&toml_str).unwrap();
    let dep = parsed.find_vcs("skills").unwrap();
    assert_eq!(dep.subdir.as_deref(), Some(".agents/skills"));
    assert_eq!(
        dep.subtree.as_deref(),
        Some("ffffffffffffffffffffffffffffffffffffffff")
    );
}

#[test]
fn test_non_vendored_vcs_subdir_serialization_roundtrip() {
    let mut lockfile = Lockfile::new();
    lockfile
        .upsert_vcs(
            "generated-skills".to_string(),
            LockedVcsDependency {
                url: "https://github.com/cuenv/cuenv.git".to_string(),
                reference: "0.27.1".to_string(),
                commit: "0123456789abcdef0123456789abcdef01234567".to_string(),
                tree: "89abcdef012345670123456789abcdef01234567".to_string(),
                vendor: false,
                path: ".cuenv/vcs/generated-skills".to_string(),
                subdir: Some(".agents/skills".to_string()),
                subtree: Some("ffffffffffffffffffffffffffffffffffffffff".to_string()),
                overlay: false,
                children: BTreeMap::new(),
            },
        )
        .unwrap();

    let toml_str = toml::to_string_pretty(&lockfile).unwrap();
    assert!(toml_str.contains("vendor = false"));
    assert!(toml_str.contains("subdir = \".agents/skills\""));

    let parsed: Lockfile = toml::from_str(&toml_str).unwrap();
    let dep = parsed.find_vcs("generated-skills").unwrap();
    assert!(!dep.vendor);
    assert_eq!(dep.subdir.as_deref(), Some(".agents/skills"));
    assert_eq!(
        dep.subtree.as_deref(),
        Some("ffffffffffffffffffffffffffffffffffffffff")
    );
}

#[test]
fn test_vcs_without_subdir_omits_fields() {
    let mut lockfile = Lockfile::new();
    lockfile
        .upsert_vcs(
            "plain".to_string(),
            LockedVcsDependency {
                url: "https://github.com/example/plain.git".to_string(),
                reference: "main".to_string(),
                commit: "0123456789abcdef0123456789abcdef01234567".to_string(),
                tree: "89abcdef012345670123456789abcdef01234567".to_string(),
                vendor: true,
                path: "vendor/plain".to_string(),
                subdir: None,
                subtree: None,
                overlay: false,
                children: BTreeMap::new(),
            },
        )
        .unwrap();

    let toml_str = toml::to_string_pretty(&lockfile).unwrap();
    assert!(!toml_str.contains("subdir"));
    assert!(!toml_str.contains("subtree"));

    let parsed: Lockfile = toml::from_str(&toml_str).unwrap();
    let dep = parsed.find_vcs("plain").unwrap();
    assert_eq!(dep.subdir, None);
    assert_eq!(dep.subtree, None);
}

#[test]
fn test_legacy_vcs_entry_without_subdir_loads() {
    // A lockfile written before subdir/subtree existed should still load.
    let legacy = r#"
version = 4

[vcs.legacy]
url = "https://github.com/example/legacy.git"
reference = "main"
commit = "0123456789abcdef0123456789abcdef01234567"
tree = "89abcdef012345670123456789abcdef01234567"
vendor = true
path = "vendor/legacy"
"#;
    let parsed: Lockfile = toml::from_str(legacy).expect("legacy lockfile parses");
    let dep = parsed.find_vcs("legacy").expect("entry present");
    assert_eq!(dep.subdir, None);
    assert_eq!(dep.subtree, None);
}

#[test]
fn test_lockfile_parent_for_sync_handles_relative_path() {
    assert_eq!(
        lockfile_parent_for_sync(Path::new("cuenv.lock")),
        Path::new(".")
    );
    assert_eq!(
        lockfile_parent_for_sync(Path::new("nested/cuenv.lock")),
        Path::new("nested")
    );
}

#[test]
fn test_vcs_path_rejects_internal_paths() {
    assert!(validate_locked_vcs_path(".git/hooks").is_err());
    assert!(validate_locked_vcs_path("vendor/.git/hooks").is_err());
    assert!(validate_locked_vcs_path(".cuenv/vcs/cache/lib").is_err());
    assert!(validate_locked_vcs_path(".cuenv/vcs/tmp/lib").is_err());
    assert!(validate_locked_vcs_path("vendor/lib").is_ok());
}

#[test]
fn test_vcs_subdir_allows_dotcuenv_paths_but_rejects_dotgit() {
    // subdir is a path *inside the remote repo*, so .cuenv/... is allowed —
    // only local-disk materialization paths reserve those prefixes.
    assert!(validate_locked_vcs_subdir(".cuenv/vcs/cache").is_ok());
    assert!(validate_locked_vcs_subdir(".cuenv/some/skill").is_ok());
    assert!(validate_locked_vcs_subdir(".agents/skills").is_ok());

    // .git inside a tree is still impossible under git's own rules.
    assert!(validate_locked_vcs_subdir(".git").is_err());
    assert!(validate_locked_vcs_subdir("nested/.git").is_err());

    // Component-safety rules still apply.
    assert!(validate_locked_vcs_subdir("--stdin").is_err());
    assert!(validate_locked_vcs_subdir("nested/-evil").is_err());
    assert!(validate_locked_vcs_subdir("a\\b").is_err());
    assert!(validate_locked_vcs_subdir("..").is_err());
    assert!(validate_locked_vcs_subdir("").is_err());
}

#[test]
fn test_upsert_runtime_validation_invalid_digest() {
    let mut lockfile = Lockfile::new();

    let result = lockfile.upsert_runtime(
        ".".to_string(),
        LockedRuntime::Nix(LockedNixRuntime {
            flake: ".".to_string(),
            output: None,
            digest: "invalid".to_string(),
            lockfile: "flake.lock".to_string(),
        }),
    );

    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("digest must start with")
    );
}

#[test]
fn test_find_tool() {
    let mut lockfile = Lockfile::new();
    lockfile
            .upsert_tool_platform(
                "jq",
                "1.7.1",
                "darwin-arm64",
                LockedToolPlatform {
                    provider: "github".to_string(),
                    digest: "sha256:abc123".to_string(),
                    source: serde_json::json!({ "repo": "jqlang/jq", "tag": "jq-1.7.1", "asset": "jq-macos-arm64" }),
                    size: None,
                    dependencies: vec![],
                },
            )
            .unwrap();

    assert!(lockfile.find_tool("jq").is_some());
    assert!(lockfile.find_tool("yq").is_none());
}

#[test]
fn test_upsert_tool_platform() {
    let mut lockfile = Lockfile::new();

    // Add first platform
    lockfile
        .upsert_tool_platform(
            "bun",
            "1.3.5",
            "darwin-arm64",
            LockedToolPlatform {
                provider: "github".to_string(),
                digest: "sha256:aaa".to_string(),
                source: serde_json::json!({ "url": "https://..." }),
                size: None,
                dependencies: vec![],
            },
        )
        .unwrap();

    assert_eq!(lockfile.tools.len(), 1);
    assert_eq!(lockfile.tools["bun"].platforms.len(), 1);

    // Add second platform
    lockfile
        .upsert_tool_platform(
            "bun",
            "1.3.5",
            "linux-x86_64",
            LockedToolPlatform {
                provider: "oci".to_string(),
                digest: "sha256:bbb".to_string(),
                source: serde_json::json!({ "image": "oven/bun:1.3.5" }),
                size: None,
                dependencies: vec![],
            },
        )
        .unwrap();

    assert_eq!(lockfile.tools.len(), 1);
    assert_eq!(lockfile.tools["bun"].platforms.len(), 2);
    assert_eq!(
        lockfile.tools["bun"].platforms["darwin-arm64"].provider,
        "github"
    );
    assert_eq!(
        lockfile.tools["bun"].platforms["linux-x86_64"].provider,
        "oci"
    );
}

#[test]
fn test_upsert_tool_platform_invalid_digest() {
    let mut lockfile = Lockfile::new();

    let result = lockfile.upsert_tool_platform(
        "jq",
        "1.7.1",
        "darwin-arm64",
        LockedToolPlatform {
            provider: "github".to_string(),
            digest: "invalid".to_string(), // Missing sha256: prefix
            source: serde_json::json!({}),
            size: None,
            dependencies: vec![],
        },
    );

    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("Invalid digest format")
    );
}

#[test]
fn test_upsert_tool_validation_empty_platforms() {
    let mut lockfile = Lockfile::new();

    let tool = LockedTool {
        version: "1.7.1".to_string(),
        platforms: BTreeMap::new(), // Empty - should fail
    };

    let result = lockfile.upsert_tool("jq".to_string(), tool);
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("at least one platform")
    );
}

#[test]
fn test_tool_names() {
    let mut lockfile = Lockfile::new();

    lockfile
        .upsert_tool_platform(
            "jq",
            "1.7.1",
            "darwin-arm64",
            LockedToolPlatform {
                provider: "github".to_string(),
                digest: "sha256:abc".to_string(),
                source: serde_json::json!({}),
                size: None,
                dependencies: vec![],
            },
        )
        .unwrap();

    lockfile
        .upsert_tool_platform(
            "yq",
            "4.44.6",
            "darwin-arm64",
            LockedToolPlatform {
                provider: "github".to_string(),
                digest: "sha256:def".to_string(),
                source: serde_json::json!({}),
                size: None,
                dependencies: vec![],
            },
        )
        .unwrap();

    let names = lockfile.tool_names();
    assert_eq!(names.len(), 2);
    assert!(names.contains(&"jq"));
    assert!(names.contains(&"yq"));
}

fn overlay_dep(children: BTreeMap<String, String>) -> LockedVcsDependency {
    LockedVcsDependency {
        url: "https://github.com/cuenv/cuenv.git".to_string(),
        reference: "main".to_string(),
        commit: "0123456789abcdef0123456789abcdef01234567".to_string(),
        tree: "89abcdef012345670123456789abcdef01234567".to_string(),
        vendor: false,
        path: ".agents/skills".to_string(),
        subdir: Some(".agents/skills".to_string()),
        subtree: Some("ffffffffffffffffffffffffffffffffffffffff".to_string()),
        overlay: true,
        children,
    }
}

#[test]
fn test_overlay_lockfile_roundtrip_and_validate() {
    let mut children = BTreeMap::new();
    children.insert(
        "example".to_string(),
        "1111111111111111111111111111111111111111".to_string(),
    );
    children.insert(
        "other".to_string(),
        "2222222222222222222222222222222222222222".to_string(),
    );
    let mut lockfile = Lockfile::new();
    lockfile
        .upsert_vcs("skills".to_string(), overlay_dep(children))
        .expect("overlay dep with children validates");

    let toml_str = toml::to_string_pretty(&lockfile).unwrap();
    assert!(toml_str.contains("overlay = true"));
    assert!(toml_str.contains("example"));

    let parsed: Lockfile = toml::from_str(&toml_str).unwrap();
    let dep = parsed.find_vcs("skills").unwrap();
    assert!(dep.overlay);
    assert_eq!(dep.children.len(), 2);
}

#[test]
fn test_overlay_requires_children() {
    let mut lockfile = Lockfile::new();
    let err = lockfile
        .upsert_vcs("skills".to_string(), overlay_dep(BTreeMap::new()))
        .expect_err("overlay without children is rejected");
    assert!(err.to_string().contains("at least one child"));
}

#[test]
fn test_overlay_rejects_vendor() {
    let mut children = BTreeMap::new();
    children.insert(
        "example".to_string(),
        "1111111111111111111111111111111111111111".to_string(),
    );
    let mut dep = overlay_dep(children);
    dep.vendor = true;
    let mut lockfile = Lockfile::new();
    let err = lockfile
        .upsert_vcs("skills".to_string(), dep)
        .expect_err("overlay with vendor is rejected");
    assert!(err.to_string().contains("incompatible with vendor"));
}

#[test]
fn test_overlay_rejects_bad_child_name() {
    let mut children = BTreeMap::new();
    children.insert(
        "nested/child".to_string(),
        "1111111111111111111111111111111111111111".to_string(),
    );
    let mut lockfile = Lockfile::new();
    let err = lockfile
        .upsert_vcs("skills".to_string(), overlay_dep(children))
        .expect_err("multi-component child name is rejected");
    assert!(err.to_string().contains("single path component"));
}

#[test]
fn test_overlay_rejects_non_hex_child_tree() {
    let mut children = BTreeMap::new();
    children.insert("example".to_string(), "not-a-sha".to_string());
    let mut lockfile = Lockfile::new();
    let err = lockfile
        .upsert_vcs("skills".to_string(), overlay_dep(children))
        .expect_err("non-hex child tree is rejected");
    assert!(err.to_string().contains("Git object ID"));
}

#[test]
fn test_children_rejected_without_overlay() {
    let mut children = BTreeMap::new();
    children.insert(
        "example".to_string(),
        "1111111111111111111111111111111111111111".to_string(),
    );
    let mut dep = overlay_dep(children);
    dep.overlay = false;
    let mut lockfile = Lockfile::new();
    let err = lockfile
        .upsert_vcs("skills".to_string(), dep)
        .expect_err("children without overlay are rejected");
    assert!(err.to_string().contains("only be set when overlay"));
}
