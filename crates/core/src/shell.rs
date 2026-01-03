//! Shell type definitions and utilities for cuenv
//!
//! This module provides shell detection and formatting utilities
//! used across cuenv for shell integration features.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Supported shell types for environment integration
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Shell {
    /// Bash shell
    #[default]
    Bash,
    /// Z shell
    Zsh,
    /// Fish shell
    Fish,
    /// PowerShell/pwsh
    #[serde(rename = "powershell")]
    PowerShell,
}

impl Shell {
    /// Detect shell from environment or argument
    pub fn detect(target: Option<&str>) -> Self {
        if let Some(t) = target {
            return Self::parse(t);
        }

        // Try to detect from environment
        if let Ok(shell) = std::env::var("SHELL") {
            if shell.contains("fish") {
                return Shell::Fish;
            } else if shell.contains("zsh") {
                return Shell::Zsh;
            } else if shell.contains("bash") {
                return Shell::Bash;
            }
        }

        // Default to bash
        Shell::Bash
    }

    /// Parse shell from string
    pub fn parse(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "zsh" => Shell::Zsh,
            "fish" => Shell::Fish,
            "powershell" | "pwsh" => Shell::PowerShell,
            _ => Shell::Bash,
        }
    }

    /// Get the name of the shell
    pub fn name(&self) -> &'static str {
        match self {
            Shell::Bash => "bash",
            Shell::Zsh => "zsh",
            Shell::Fish => "fish",
            Shell::PowerShell => "powershell",
        }
    }

    /// Check if this shell is supported for integration
    pub fn is_supported(&self) -> bool {
        matches!(self, Shell::Bash | Shell::Zsh | Shell::Fish)
    }
}

impl fmt::Display for Shell {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shell_default() {
        let shell = Shell::default();
        assert_eq!(shell, Shell::Bash);
    }

    #[test]
    fn test_shell_parse() {
        assert_eq!(Shell::parse("bash"), Shell::Bash);
        assert_eq!(Shell::parse("zsh"), Shell::Zsh);
        assert_eq!(Shell::parse("fish"), Shell::Fish);
        assert_eq!(Shell::parse("powershell"), Shell::PowerShell);
        assert_eq!(Shell::parse("pwsh"), Shell::PowerShell);
        assert_eq!(Shell::parse("unknown"), Shell::Bash);
    }

    #[test]
    fn test_shell_parse_case_insensitive() {
        assert_eq!(Shell::parse("BASH"), Shell::Bash);
        assert_eq!(Shell::parse("ZSH"), Shell::Zsh);
        assert_eq!(Shell::parse("Fish"), Shell::Fish);
        assert_eq!(Shell::parse("PowerShell"), Shell::PowerShell);
        assert_eq!(Shell::parse("PWSH"), Shell::PowerShell);
    }

    #[test]
    fn test_shell_detect_with_target() {
        assert_eq!(Shell::detect(Some("bash")), Shell::Bash);
        assert_eq!(Shell::detect(Some("zsh")), Shell::Zsh);
        assert_eq!(Shell::detect(Some("fish")), Shell::Fish);
        assert_eq!(Shell::detect(Some("powershell")), Shell::PowerShell);
    }

    #[test]
    fn test_shell_detect_from_env_fish() {
        temp_env::with_var("SHELL", Some("/usr/bin/fish"), || {
            let shell = Shell::detect(None);
            assert_eq!(shell, Shell::Fish);
        });
    }

    #[test]
    fn test_shell_detect_from_env_zsh() {
        temp_env::with_var("SHELL", Some("/bin/zsh"), || {
            let shell = Shell::detect(None);
            assert_eq!(shell, Shell::Zsh);
        });
    }

    #[test]
    fn test_shell_detect_from_env_bash() {
        temp_env::with_var("SHELL", Some("/bin/bash"), || {
            let shell = Shell::detect(None);
            assert_eq!(shell, Shell::Bash);
        });
    }

    #[test]
    fn test_shell_detect_default_fallback() {
        temp_env::with_var_unset("SHELL", || {
            let shell = Shell::detect(None);
            assert_eq!(shell, Shell::Bash);
        });
    }

    #[test]
    fn test_shell_name() {
        assert_eq!(Shell::Bash.name(), "bash");
        assert_eq!(Shell::Zsh.name(), "zsh");
        assert_eq!(Shell::Fish.name(), "fish");
        assert_eq!(Shell::PowerShell.name(), "powershell");
    }

    #[test]
    fn test_shell_support() {
        assert!(Shell::Bash.is_supported());
        assert!(Shell::Zsh.is_supported());
        assert!(Shell::Fish.is_supported());
        assert!(!Shell::PowerShell.is_supported());
    }

    #[test]
    fn test_shell_display() {
        assert_eq!(format!("{}", Shell::Bash), "bash");
        assert_eq!(format!("{}", Shell::Zsh), "zsh");
        assert_eq!(format!("{}", Shell::Fish), "fish");
        assert_eq!(format!("{}", Shell::PowerShell), "powershell");
    }

    #[test]
    fn test_shell_serde_roundtrip() {
        let shells = vec![Shell::Bash, Shell::Zsh, Shell::Fish, Shell::PowerShell];
        for shell in shells {
            let json = serde_json::to_string(&shell).unwrap();
            let parsed: Shell = serde_json::from_str(&json).unwrap();
            assert_eq!(shell, parsed);
        }
    }

    #[test]
    fn test_shell_serde_powershell_rename() {
        let shell = Shell::PowerShell;
        let json = serde_json::to_string(&shell).unwrap();
        assert_eq!(json, "\"powershell\"");
    }

    #[test]
    fn test_shell_clone() {
        let shell = Shell::Fish;
        let cloned = shell;
        assert_eq!(shell, cloned);
    }

    #[test]
    fn test_shell_copy() {
        let shell = Shell::Zsh;
        let copied = shell;
        assert_eq!(shell, copied);
    }

    #[test]
    fn test_shell_debug() {
        let debug = format!("{:?}", Shell::Bash);
        assert!(debug.contains("Bash"));
    }
}
