use cuenv_core::lockfile::Lockfile;
use cuenv_core::tools::{
    Platform, ResolvedToolActivationStep, ToolActivationOperation, ToolActivationResolveOptions,
    resolve_tool_activation,
};
use std::path::{Path, PathBuf};

const NO_TOOLS_CONFIGURED_LINES: &[&str] = &[
    "No tools configured.",
    "",
    "To add tools, create a runtime in your env.cue:",
    "",
    "  runtime: #ToolsRuntime & {",
    "      platforms: [\"darwin-arm64\", \"linux-x86_64\"]",
    "      tools: {",
    "          jq: \"1.7.1\"",
    "          yq: \"4.44.6\"",
    "          foundationdb: {",
    "              version: \"7.3.63\"",
    "              source: #GitHub & {repo: \"apple/foundationdb\", asset: \"FoundationDB-{version}_arm64.pkg\", extract: [{kind: \"lib\", path: \"libfdb_c.dylib\", env: \"FDB_CLIENT_LIB\"}]}",
    "          }",
    "      }",
    "  }",
];

pub(super) fn render_tools_list(
    lockfile: &Lockfile,
    lockfile_path: &Path,
    current_platform: &str,
) -> Vec<String> {
    if lockfile.tools.is_empty() {
        return no_tools_configured_lines();
    }

    let mut lines = vec!["Configured tools:".to_string(), String::new()];

    let mut tools: Vec<_> = lockfile.tools.iter().collect();
    tools.sort_by_key(|(name, _)| *name);

    for (name, tool) in tools {
        lines.push(format!("  {} v{}", name, tool.version));

        for (platform, locked) in &tool.platforms {
            let marker = if platform == current_platform {
                " (current)"
            } else {
                ""
            };
            lines.push(format!(
                "    - {}: {} ({}){}",
                platform,
                locked.provider,
                digest_preview(&locked.digest),
                marker
            ));
        }
    }

    lines.push(String::new());
    lines.extend(activation_section_lines(lockfile, lockfile_path));
    lines.push(String::new());
    lines.push(format!(
        "Total: {} tools, {} platforms",
        lockfile.tools.len(),
        lockfile
            .tools
            .values()
            .map(|t| t.platforms.len())
            .sum::<usize>()
    ));
    lines
}

fn no_tools_configured_lines() -> Vec<String> {
    NO_TOOLS_CONFIGURED_LINES
        .iter()
        .map(|line| (*line).to_string())
        .collect()
}

fn digest_preview(digest: &str) -> &str {
    digest.get(..20).unwrap_or(digest)
}

fn activation_section_lines(lockfile: &Lockfile, lockfile_path: &Path) -> Vec<String> {
    activation_section_lines_with_cache_dir(lockfile, lockfile_path, None)
}

fn activation_section_lines_with_cache_dir(
    lockfile: &Lockfile,
    lockfile_path: &Path,
    cache_dir: Option<PathBuf>,
) -> Vec<String> {
    let platform = Platform::current();
    let mode = if lockfile.tools_activation.is_empty() {
        "inferred"
    } else {
        "explicit"
    };
    let mut lines = vec![format!("Activation ({platform}, {mode}):")];
    let mut options =
        ToolActivationResolveOptions::new(lockfile, lockfile_path).with_platform(platform);
    if let Some(cache_dir) = cache_dir {
        options = options.with_cache_dir(cache_dir);
    }

    match resolve_tool_activation(&options) {
        Ok(steps) => {
            let rendered = render_activation_steps(&steps);
            if rendered.is_empty() {
                lines.push(
                    "  - No activation paths are currently materialized for this platform."
                        .to_string(),
                );
            } else {
                lines.extend(rendered);
            }
        }
        Err(err) => lines.push(format!("  - error: {err}")),
    }

    lines
}

fn render_activation_steps(steps: &[ResolvedToolActivationStep]) -> Vec<String> {
    steps
        .iter()
        .filter(|step| !step.value.is_empty() || matches!(step.op, ToolActivationOperation::Set))
        .map(|step| {
            let value = if step.value.is_empty() {
                "<empty>"
            } else {
                step.value.as_str()
            };
            format!(
                "  - {} ({}): {}",
                step.var,
                activation_operation_label(&step.op),
                value
            )
        })
        .collect()
}

fn activation_operation_label(operation: &ToolActivationOperation) -> &'static str {
    match operation {
        ToolActivationOperation::Set => "set",
        ToolActivationOperation::Prepend => "prepend",
        ToolActivationOperation::Append => "append",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cuenv_core::lockfile::{LockedTool, LockedToolPlatform};
    use cuenv_core::tools::{ToolActivationSource, ToolActivationStep};
    use std::collections::BTreeMap;
    use std::fs;

    fn current_platform_key() -> String {
        Platform::current().to_string()
    }

    fn github_tool(version: &str) -> LockedTool {
        LockedTool {
            version: version.to_string(),
            platforms: BTreeMap::from([(
                current_platform_key(),
                LockedToolPlatform {
                    provider: "github".to_string(),
                    digest: "sha256:abc".to_string(),
                    source: serde_json::json!({
                        "type": "github",
                        "repo": "jqlang/jq",
                        "tag": "jq-1.7.1",
                        "asset": "jq",
                    }),
                    size: None,
                    dependencies: vec![],
                },
            )]),
        }
    }

    #[test]
    fn test_activation_section_lines_show_inferred_activation() {
        let temp = tempfile::tempdir().unwrap();
        let lockfile_path = temp.path().join("cuenv.lock");
        let cache_dir = temp.path().join("cache");
        let bin_dir = cache_dir
            .join("github")
            .join("jq")
            .join("1.7.1")
            .join("bin");
        fs::create_dir_all(&bin_dir).unwrap();

        let mut lockfile = Lockfile::new();
        lockfile
            .tools
            .insert("jq".to_string(), github_tool("1.7.1"));

        let lines =
            activation_section_lines_with_cache_dir(&lockfile, &lockfile_path, Some(cache_dir));

        assert!(
            lines
                .first()
                .is_some_and(|line| line.contains("Activation (") && line.contains("inferred"))
        );
        assert!(
            lines
                .iter()
                .any(|line| line == &format!("  - PATH (prepend): {}", bin_dir.display()))
        );
    }

    #[test]
    fn test_activation_section_lines_show_explicit_activation() {
        let temp = tempfile::tempdir().unwrap();
        let lockfile_path = temp.path().join("cuenv.lock");
        let cache_dir = temp.path().join("cache");
        let bin_dir = cache_dir
            .join("github")
            .join("jq")
            .join("1.7.1")
            .join("bin");
        fs::create_dir_all(&bin_dir).unwrap();

        let mut lockfile = Lockfile::new();
        lockfile
            .tools
            .insert("jq".to_string(), github_tool("1.7.1"));
        lockfile.tools_activation = vec![ToolActivationStep {
            var: "PATH".to_string(),
            op: ToolActivationOperation::Prepend,
            separator: ":".to_string(),
            from: ToolActivationSource::ToolBinDir {
                tool: "jq".to_string(),
            },
        }];

        let lines =
            activation_section_lines_with_cache_dir(&lockfile, &lockfile_path, Some(cache_dir));

        assert!(
            lines
                .first()
                .is_some_and(|line| line.contains("Activation (") && line.contains("explicit"))
        );
        assert_eq!(
            lines[1],
            format!("  - PATH (prepend): {}", bin_dir.display())
        );
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn test_activation_section_lines_show_invalid_activation_error() {
        let temp = tempfile::tempdir().unwrap();
        let lockfile_path = temp.path().join("cuenv.lock");
        let mut lockfile = Lockfile::new();
        lockfile
            .tools
            .insert("jq".to_string(), github_tool("1.7.1"));
        lockfile.tools_activation = vec![ToolActivationStep {
            var: "PATH".to_string(),
            op: ToolActivationOperation::Prepend,
            separator: ":".to_string(),
            from: ToolActivationSource::ToolBinDir {
                tool: "missing".to_string(),
            },
        }];

        let lines = activation_section_lines_with_cache_dir(&lockfile, &lockfile_path, None);

        assert!(
            lines
                .iter()
                .any(|line| line.contains("error:") && line.contains("unknown tool 'missing'"))
        );
    }

    #[test]
    fn test_activation_section_lines_note_when_no_paths_are_materialized() {
        let temp = tempfile::tempdir().unwrap();
        let lockfile_path = temp.path().join("cuenv.lock");
        let cache_dir = temp.path().join("cache");
        let mut lockfile = Lockfile::new();
        lockfile
            .tools
            .insert("jq".to_string(), github_tool("1.7.1"));

        let lines =
            activation_section_lines_with_cache_dir(&lockfile, &lockfile_path, Some(cache_dir));

        assert!(lines.iter().any(|line| {
            line == "  - No activation paths are currently materialized for this platform."
        }));
    }

    #[test]
    fn test_render_tools_list_handles_short_digests() {
        let temp = tempfile::tempdir().unwrap();
        let lockfile_path = temp.path().join("cuenv.lock");
        let mut lockfile = Lockfile::new();
        lockfile
            .tools
            .insert("jq".to_string(), github_tool("1.7.1"));

        let lines = render_tools_list(&lockfile, &lockfile_path, &current_platform_key());

        assert!(
            lines
                .iter()
                .any(|line| line.contains("sha256:abc") && line.contains("(current)")),
            "expected full short digest without slicing panic, got: {lines:?}"
        );
    }
}
