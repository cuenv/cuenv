//! Configuration types for cuenv
//!
//! Based on schema/config.cue

use serde::{Deserialize, Serialize};

/// Main configuration structure for cuenv
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    /// Task output format (for task execution)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_format: Option<OutputFormat>,

    /// Command-specific configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commands: Option<CommandsConfig>,

    /// Cache configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_mode: Option<CacheMode>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_enabled: Option<bool>,

    /// Security and debugging
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audit_mode: Option<bool>,

    /// Chrome trace generation
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_output: Option<bool>,

    /// Default environment settings
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_environment: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_capabilities: Option<Vec<String>>,

    /// Task backend configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backend: Option<BackendConfig>,

    /// CI-specific configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ci: Option<CIConfig>,
}

/// Command-specific configuration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct CommandsConfig {
    /// Task command configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task: Option<TaskCommandConfig>,
}

/// Task command configuration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct TaskCommandConfig {
    /// Task list configuration (for `cuenv task` without arguments)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub list: Option<TaskListConfig>,
}

/// Task list display configuration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct TaskListConfig {
    /// Output format for task listing
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<TaskListFormat>,
}

impl Config {
    /// Get the configured task list format, if any.
    ///
    /// Accesses `config.commands.task.list.format` with safe navigation.
    #[must_use]
    pub fn task_list_format(&self) -> Option<TaskListFormat> {
        self.commands
            .as_ref()
            .and_then(|c| c.task.as_ref())
            .and_then(|t| t.list.as_ref())
            .and_then(|l| l.format)
    }
}

/// CI-specific configuration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct CIConfig {
    /// Cuenv installation configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cuenv: Option<CuenvConfig>,
}

/// Configuration for cuenv installation in CI
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CuenvConfig {
    /// Source for cuenv binary
    #[serde(default)]
    pub source: CuenvSource,

    /// Version to install ("self", "latest", or specific version like "0.17.0")
    #[serde(default = "default_cuenv_version")]
    pub version: String,
}

fn default_cuenv_version() -> String {
    "self".to_string()
}

impl Default for CuenvConfig {
    fn default() -> Self {
        Self {
            source: CuenvSource::Release,
            version: default_cuenv_version(),
        }
    }
}

/// Source for cuenv binary in CI environments
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum CuenvSource {
    /// Build using native Rust/Go toolchains (no Nix)
    Native,
    /// Use pre-built artifact from earlier CI job
    Artifact,
    /// Build from git checkout (requires Nix)
    Git,
    /// Install via Nix flake (auto-configures Cachix)
    Nix,
    /// Install via Homebrew tap (no Nix required)
    Homebrew,
    /// Download from GitHub Releases (default)
    #[default]
    Release,
}

impl CuenvSource {
    /// Get the string representation for activation condition matching
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Artifact => "artifact",
            Self::Git => "git",
            Self::Nix => "nix",
            Self::Homebrew => "homebrew",
            Self::Release => "release",
        }
    }
}

/// Task output format options (for task execution)
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum OutputFormat {
    Tui,
    Spinner,
    Simple,
    Tree,
    Json,
}

/// Task list format options (for `cuenv task` without arguments)
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TaskListFormat {
    /// Plain tree structure (default for non-TTY)
    Text,
    /// Colored tree structure (default for TTY)
    Rich,
    /// Category-grouped bordered tables
    Tables,
    /// Status dashboard with cache indicators
    Dashboard,
    /// Emoji-prefixed semantic categories
    Emoji,
}

impl TaskListFormat {
    /// Convert to the string representation used by the task command
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Rich => "rich",
            Self::Tables => "tables",
            Self::Dashboard => "dashboard",
            Self::Emoji => "emoji",
        }
    }
}

/// Cache mode options
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum CacheMode {
    Off,
    Read,
    ReadWrite,
    Write,
}

fn default_backend_type() -> String {
    "host".to_string()
}

/// Backend configuration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct BackendConfig {
    /// Which backend to use for tasks
    #[serde(default = "default_backend_type", rename = "type")]
    pub backend_type: String,

    /// Backend-specific options
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<BackendOptions>,
}

/// Backend-specific options supported by cuenv
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct BackendOptions {
    /// Default container image for the Dagger backend
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,

    /// Optional platform hint for the Dagger backend
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_default() {
        let config = Config::default();
        assert!(config.output_format.is_none());
        assert!(config.commands.is_none());
        assert!(config.cache_mode.is_none());
        assert!(config.cache_enabled.is_none());
    }

    #[test]
    fn test_config_serde_roundtrip() {
        let config = Config {
            output_format: Some(OutputFormat::Json),
            commands: None,
            cache_mode: Some(CacheMode::ReadWrite),
            cache_enabled: Some(true),
            audit_mode: Some(false),
            trace_output: None,
            default_environment: Some("dev".to_string()),
            default_capabilities: Some(vec!["cap1".to_string()]),
            backend: None,
            ci: None,
        };
        let json = serde_json::to_string(&config).unwrap();
        let parsed: Config = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, config);
    }

    #[test]
    fn test_config_skip_none_fields() {
        let config = Config::default();
        let json = serde_json::to_string(&config).unwrap();
        assert_eq!(json, "{}");
    }

    #[test]
    fn test_config_task_list_format_none() {
        let config = Config::default();
        assert!(config.task_list_format().is_none());
    }

    #[test]
    fn test_config_task_list_format_some() {
        let config = Config {
            commands: Some(CommandsConfig {
                task: Some(TaskCommandConfig {
                    list: Some(TaskListConfig {
                        format: Some(TaskListFormat::Dashboard),
                    }),
                }),
            }),
            ..Default::default()
        };
        assert_eq!(config.task_list_format(), Some(TaskListFormat::Dashboard));
    }

    #[test]
    fn test_config_task_list_format_partial_none() {
        let config = Config {
            commands: Some(CommandsConfig { task: None }),
            ..Default::default()
        };
        assert!(config.task_list_format().is_none());
    }

    #[test]
    fn test_output_format_serde() {
        assert_eq!(
            serde_json::to_string(&OutputFormat::Tui).unwrap(),
            r#""tui""#
        );
        assert_eq!(
            serde_json::to_string(&OutputFormat::Spinner).unwrap(),
            r#""spinner""#
        );
        assert_eq!(
            serde_json::to_string(&OutputFormat::Simple).unwrap(),
            r#""simple""#
        );
        assert_eq!(
            serde_json::to_string(&OutputFormat::Tree).unwrap(),
            r#""tree""#
        );
        assert_eq!(
            serde_json::to_string(&OutputFormat::Json).unwrap(),
            r#""json""#
        );
    }

    #[test]
    fn test_output_format_deserialize() {
        assert_eq!(
            serde_json::from_str::<OutputFormat>(r#""tui""#).unwrap(),
            OutputFormat::Tui
        );
        assert_eq!(
            serde_json::from_str::<OutputFormat>(r#""json""#).unwrap(),
            OutputFormat::Json
        );
    }

    #[test]
    fn test_task_list_format_as_str() {
        assert_eq!(TaskListFormat::Text.as_str(), "text");
        assert_eq!(TaskListFormat::Rich.as_str(), "rich");
        assert_eq!(TaskListFormat::Tables.as_str(), "tables");
        assert_eq!(TaskListFormat::Dashboard.as_str(), "dashboard");
        assert_eq!(TaskListFormat::Emoji.as_str(), "emoji");
    }

    #[test]
    fn test_task_list_format_serde() {
        assert_eq!(
            serde_json::to_string(&TaskListFormat::Tables).unwrap(),
            r#""tables""#
        );
        assert_eq!(
            serde_json::from_str::<TaskListFormat>(r#""rich""#).unwrap(),
            TaskListFormat::Rich
        );
    }

    #[test]
    fn test_cache_mode_serde() {
        assert_eq!(serde_json::to_string(&CacheMode::Off).unwrap(), r#""off""#);
        assert_eq!(
            serde_json::to_string(&CacheMode::Read).unwrap(),
            r#""read""#
        );
        assert_eq!(
            serde_json::to_string(&CacheMode::ReadWrite).unwrap(),
            r#""read-write""#
        );
        assert_eq!(
            serde_json::to_string(&CacheMode::Write).unwrap(),
            r#""write""#
        );
    }

    #[test]
    fn test_cache_mode_deserialize() {
        assert_eq!(
            serde_json::from_str::<CacheMode>(r#""off""#).unwrap(),
            CacheMode::Off
        );
        assert_eq!(
            serde_json::from_str::<CacheMode>(r#""read-write""#).unwrap(),
            CacheMode::ReadWrite
        );
    }

    #[test]
    fn test_cuenv_source_default() {
        assert_eq!(CuenvSource::default(), CuenvSource::Release);
    }

    #[test]
    fn test_cuenv_source_as_str() {
        assert_eq!(CuenvSource::Native.as_str(), "native");
        assert_eq!(CuenvSource::Artifact.as_str(), "artifact");
        assert_eq!(CuenvSource::Git.as_str(), "git");
        assert_eq!(CuenvSource::Nix.as_str(), "nix");
        assert_eq!(CuenvSource::Homebrew.as_str(), "homebrew");
        assert_eq!(CuenvSource::Release.as_str(), "release");
    }

    #[test]
    fn test_cuenv_source_serde() {
        assert_eq!(
            serde_json::to_string(&CuenvSource::Nix).unwrap(),
            r#""nix""#
        );
        assert_eq!(
            serde_json::from_str::<CuenvSource>(r#""homebrew""#).unwrap(),
            CuenvSource::Homebrew
        );
    }

    #[test]
    fn test_cuenv_config_default() {
        let config = CuenvConfig::default();
        assert_eq!(config.source, CuenvSource::Release);
        assert_eq!(config.version, "self");
    }

    #[test]
    fn test_cuenv_config_serde() {
        let config = CuenvConfig {
            source: CuenvSource::Nix,
            version: "0.20.0".to_string(),
        };
        let json = serde_json::to_string(&config).unwrap();
        let parsed: CuenvConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.source, CuenvSource::Nix);
        assert_eq!(parsed.version, "0.20.0");
    }

    #[test]
    fn test_ci_config_default() {
        let config = CIConfig::default();
        assert!(config.cuenv.is_none());
    }

    #[test]
    fn test_ci_config_with_cuenv() {
        let config = CIConfig {
            cuenv: Some(CuenvConfig::default()),
        };
        assert!(config.cuenv.is_some());
        assert_eq!(config.cuenv.as_ref().unwrap().version, "self");
    }

    #[test]
    fn test_backend_config_default() {
        let config = BackendConfig::default();
        assert_eq!(config.backend_type, "");
        assert!(config.options.is_none());
    }

    #[test]
    fn test_backend_config_serde_default_type() {
        let json = r#"{}"#;
        let config: BackendConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.backend_type, "host");
    }

    #[test]
    fn test_backend_config_with_options() {
        let config = BackendConfig {
            backend_type: "dagger".to_string(),
            options: Some(BackendOptions {
                image: Some("alpine:latest".to_string()),
                platform: Some("linux/amd64".to_string()),
            }),
        };
        let json = serde_json::to_string(&config).unwrap();
        assert!(json.contains("dagger"));
        assert!(json.contains("alpine:latest"));
    }

    #[test]
    fn test_backend_options_default() {
        let opts = BackendOptions::default();
        assert!(opts.image.is_none());
        assert!(opts.platform.is_none());
    }

    #[test]
    fn test_commands_config_default() {
        let config = CommandsConfig::default();
        assert!(config.task.is_none());
    }

    #[test]
    fn test_task_command_config_default() {
        let config = TaskCommandConfig::default();
        assert!(config.list.is_none());
    }

    #[test]
    fn test_task_list_config_default() {
        let config = TaskListConfig::default();
        assert!(config.format.is_none());
    }

    #[test]
    fn test_full_config_serde() {
        let config = Config {
            output_format: Some(OutputFormat::Tui),
            commands: Some(CommandsConfig {
                task: Some(TaskCommandConfig {
                    list: Some(TaskListConfig {
                        format: Some(TaskListFormat::Emoji),
                    }),
                }),
            }),
            cache_mode: Some(CacheMode::ReadWrite),
            cache_enabled: Some(true),
            audit_mode: Some(true),
            trace_output: Some(true),
            default_environment: Some("production".to_string()),
            default_capabilities: Some(vec!["admin".to_string(), "write".to_string()]),
            backend: Some(BackendConfig {
                backend_type: "dagger".to_string(),
                options: Some(BackendOptions {
                    image: Some("node:18".to_string()),
                    platform: None,
                }),
            }),
            ci: Some(CIConfig {
                cuenv: Some(CuenvConfig {
                    source: CuenvSource::Git,
                    version: "latest".to_string(),
                }),
            }),
        };
        let json = serde_json::to_string_pretty(&config).unwrap();
        let parsed: Config = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, config);
    }

    #[test]
    fn test_config_clone() {
        let config = Config {
            output_format: Some(OutputFormat::Json),
            ..Default::default()
        };
        let cloned = config.clone();
        assert_eq!(cloned.output_format, Some(OutputFormat::Json));
    }

    #[test]
    fn test_output_format_equality() {
        assert_eq!(OutputFormat::Tui, OutputFormat::Tui);
        assert_ne!(OutputFormat::Tui, OutputFormat::Json);
    }

    #[test]
    fn test_cache_mode_equality() {
        assert_eq!(CacheMode::ReadWrite, CacheMode::ReadWrite);
        assert_ne!(CacheMode::Read, CacheMode::Write);
    }
}
