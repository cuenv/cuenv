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
    fn test_shell_detection() {
        assert_eq!(Shell::parse("bash"), Shell::Bash);
        assert_eq!(Shell::parse("zsh"), Shell::Zsh);
        assert_eq!(Shell::parse("fish"), Shell::Fish);
        assert_eq!(Shell::parse("powershell"), Shell::PowerShell);
        assert_eq!(Shell::parse("pwsh"), Shell::PowerShell);
        assert_eq!(Shell::parse("unknown"), Shell::Bash);
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
}
