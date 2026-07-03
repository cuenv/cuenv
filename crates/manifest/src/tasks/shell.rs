use serde::{Deserialize, Serialize};
use std::path::Path;

// =============================================================================
// Script Shell Configuration
// =============================================================================

/// Shell interpreter for script-based tasks
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum ScriptShell {
    /// GNU Bash (default)
    #[default]
    Bash,
    /// POSIX sh
    Sh,
    /// Z shell
    Zsh,
    /// Fish shell
    Fish,
    /// Nushell
    Nu,
    /// Windows PowerShell
    Powershell,
    /// PowerShell Core
    Pwsh,
    /// Python interpreter
    Python,
    /// Node.js interpreter
    Node,
    /// Ruby interpreter
    Ruby,
    /// Perl interpreter
    Perl,
}

impl ScriptShell {
    /// Get the command and flag for this shell
    #[must_use]
    pub fn command_and_flag(&self) -> (&'static str, &'static str) {
        match self {
            Self::Bash => ("bash", "-c"),
            Self::Sh => ("sh", "-c"),
            Self::Zsh => ("zsh", "-c"),
            Self::Fish => ("fish", "-c"),
            Self::Nu => ("nu", "-c"),
            Self::Powershell => ("powershell", "-Command"),
            Self::Pwsh => ("pwsh", "-Command"),
            Self::Python => ("python", "-c"),
            Self::Node => ("node", "-e"),
            Self::Ruby => ("ruby", "-e"),
            Self::Perl => ("perl", "-e"),
        }
    }

    /// Returns true if this shell supports POSIX-style options (errexit, pipefail, etc.)
    #[must_use]
    pub fn supports_shell_options(&self) -> bool {
        matches!(self, Self::Bash | Self::Sh | Self::Zsh)
    }

    /// Returns true if this shell supports `set -o pipefail`.
    #[must_use]
    pub fn supports_pipefail(&self) -> bool {
        matches!(self, Self::Bash | Self::Zsh)
    }

    /// Parse a shell enum from an executable path or bare command name.
    #[must_use]
    pub fn from_command(command: &str) -> Option<Self> {
        let file_name = Path::new(command)
            .file_name()?
            .to_str()?
            .to_ascii_lowercase();
        let normalized = file_name.strip_suffix(".exe").unwrap_or(&file_name);

        match normalized {
            "bash" => Some(Self::Bash),
            "sh" => Some(Self::Sh),
            "zsh" => Some(Self::Zsh),
            "fish" => Some(Self::Fish),
            "nu" => Some(Self::Nu),
            "powershell" => Some(Self::Powershell),
            "pwsh" => Some(Self::Pwsh),
            "python" => Some(Self::Python),
            "node" => Some(Self::Node),
            "ruby" => Some(Self::Ruby),
            "perl" => Some(Self::Perl),
            _ => None,
        }
    }
}

/// A shell option toggle.
///
/// Serializes as a plain boolean, so the CUE schema wire format is
/// unchanged.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(from = "bool", into = "bool")]
pub enum ShellOptionToggle {
    /// The option is enabled (its `set` flag is emitted).
    Enabled,
    /// The option is disabled.
    #[default]
    Disabled,
}

impl ShellOptionToggle {
    /// Whether the option is enabled.
    #[must_use]
    pub const fn is_enabled(self) -> bool {
        matches!(self, Self::Enabled)
    }
}

impl From<bool> for ShellOptionToggle {
    fn from(enabled: bool) -> Self {
        if enabled {
            Self::Enabled
        } else {
            Self::Disabled
        }
    }
}

impl From<ShellOptionToggle> for bool {
    fn from(toggle: ShellOptionToggle) -> Self {
        toggle.is_enabled()
    }
}

/// Shell options for bash-like shells
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShellOptions {
    /// -e: exit on error (default: enabled)
    #[serde(default = "default_enabled")]
    pub errexit: ShellOptionToggle,
    /// -u: error on undefined vars (default: enabled)
    #[serde(default = "default_enabled")]
    pub nounset: ShellOptionToggle,
    /// -o pipefail: fail on pipe errors (default: enabled, requires bash or zsh)
    #[serde(default = "default_enabled")]
    pub pipefail: ShellOptionToggle,
    /// -x: debug/trace mode (default: disabled)
    #[serde(default)]
    pub xtrace: ShellOptionToggle,
}

fn default_enabled() -> ShellOptionToggle {
    ShellOptionToggle::Enabled
}

impl Default for ShellOptions {
    fn default() -> Self {
        Self {
            errexit: ShellOptionToggle::Enabled,
            nounset: ShellOptionToggle::Enabled,
            pipefail: ShellOptionToggle::Enabled,
            xtrace: ShellOptionToggle::Disabled,
        }
    }
}

impl ShellOptions {
    /// Generate the shell options prefix for a script
    #[must_use]
    pub fn to_set_commands(&self) -> String {
        let mut opts = Vec::new();
        if self.errexit.is_enabled() {
            opts.push("-e");
        }
        if self.nounset.is_enabled() {
            opts.push("-u");
        }
        if self.pipefail.is_enabled() {
            opts.push("-o pipefail");
        }
        if self.xtrace.is_enabled() {
            opts.push("-x");
        }
        if opts.is_empty() {
            String::new()
        } else {
            format!("set {}\n", opts.join(" "))
        }
    }
}

/// Shell configuration for task execution (legacy, for backwards compatibility)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Shell {
    /// Shell executable name (e.g., "bash", "fish", "zsh")
    pub command: Option<String>,
    /// Flag for command execution (e.g., "-c", "--command")
    pub flag: Option<String>,
}
