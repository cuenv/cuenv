use super::*;
use cuenv_core::lockfile::{LOCKFILE_VERSION, LockedToolPlatform, LockedVcsDependency};
use cuenv_core::manifest::SourceConfig;
use cuenv_core::tools::{
    Arch, Os, ToolActivationOperation, ToolActivationSource, ToolActivationStep,
};

#[test]
fn test_seed_lockfile_preserves_tools_activation_and_resets_generated_sections() {
    let mut existing = Lockfile::new();
    existing.version = 1;
    existing.tools.insert(
        "jq".to_string(),
        LockedTool {
            version: "1.7.1".to_string(),
            platforms: BTreeMap::from([(
                "linux-x86_64".to_string(),
                LockedToolPlatform {
                    provider: "github".to_string(),
                    digest: "sha256:abc".to_string(),
                    source: serde_json::json!({"repo": "jqlang/jq"}),
                    size: None,
                    dependencies: vec![],
                },
            )]),
        },
    );
    existing.tools_activation.push(ToolActivationStep {
        var: "PATH".to_string(),
        op: ToolActivationOperation::Prepend,
        separator: ":".to_string(),
        from: ToolActivationSource::AllBinDirs,
    });
    existing.artifacts.push(LockedArtifact {
        kind: ArtifactKind::Image {
            image: "nginx:1.25-alpine".to_string(),
            extract: vec![],
        },
        platforms: BTreeMap::from([(
            "linux-x86_64".to_string(),
            PlatformData {
                digest: "sha256:def".to_string(),
                size: None,
            },
        )]),
    });
    existing.vcs.insert(
        "lib".to_string(),
        LockedVcsDependency {
            url: "https://example.com/lib.git".to_string(),
            reference: "main".to_string(),
            commit: "0123456789abcdef0123456789abcdef01234567".to_string(),
            tree: "89abcdef012345670123456789abcdef01234567".to_string(),
            vendor: true,
            path: "vendor/lib".to_string(),
            subdir: None,
            subtree: None,
        },
    );

    let seeded = seed_lockfile(Some(&existing), LockfileSeedMode::ResetGeneratedSections);

    assert_eq!(seeded.version, LOCKFILE_VERSION);
    assert_eq!(seeded.tools_activation, existing.tools_activation);
    assert_eq!(seeded.vcs, existing.vcs);
    assert!(seeded.tools.is_empty());
    assert!(seeded.artifacts.is_empty());
}

#[test]
fn test_seeded_lockfile_remains_equal_when_generated_sections_are_rebuilt() {
    let mut existing = Lockfile::new();
    existing.tools.insert(
        "jq".to_string(),
        LockedTool {
            version: "1.7.1".to_string(),
            platforms: BTreeMap::from([(
                "linux-x86_64".to_string(),
                LockedToolPlatform {
                    provider: "github".to_string(),
                    digest: "sha256:abc".to_string(),
                    source: serde_json::json!({"repo": "jqlang/jq"}),
                    size: None,
                    dependencies: vec![],
                },
            )]),
        },
    );
    existing.tools_activation.push(ToolActivationStep {
        var: "PATH".to_string(),
        op: ToolActivationOperation::Prepend,
        separator: ":".to_string(),
        from: ToolActivationSource::AllBinDirs,
    });
    existing.artifacts.push(LockedArtifact {
        kind: ArtifactKind::Image {
            image: "nginx:1.25-alpine".to_string(),
            extract: vec![],
        },
        platforms: BTreeMap::from([(
            "linux-x86_64".to_string(),
            PlatformData {
                digest: "sha256:def".to_string(),
                size: None,
            },
        )]),
    });

    let mut rebuilt = seed_lockfile(Some(&existing), LockfileSeedMode::ResetGeneratedSections);
    rebuilt.tools = existing.tools.clone();
    rebuilt.artifacts = existing.artifacts.clone();

    assert_eq!(rebuilt, existing);
}

#[test]
fn test_source_config_to_tool_source_expands_url_templates_for_cache_comparison() {
    let source = SourceConfig::Url {
        url: "https://example.com/tool-{version}-{os}-{arch}.tar.gz".to_string(),
        path: Some("tool-{os}-{arch}".to_string()),
        extract: vec![],
    };
    let platform = ToolPlatform::new(Os::Linux, Arch::Arm64);

    let (_, tool_source, source_json) = source_config_to_tool_source("1.2.3", &source, &platform);

    match tool_source {
        ToolSource::Url { url, extract } => {
            assert_eq!(url, "https://example.com/tool-1.2.3-linux-aarch64.tar.gz");
            assert_eq!(
                extract,
                vec![ToolExtract::Bin {
                    path: "tool-linux-aarch64".to_string(),
                    as_name: None,
                }]
            );
        }
        _ => panic!("expected url source"),
    }

    assert_eq!(
        source_json,
        serde_json::json!({
            "type": "url",
            "url": "https://example.com/tool-1.2.3-linux-aarch64.tar.gz",
            "extract": [{"kind": "bin", "path": "tool-linux-aarch64"}],
        })
    );
}
