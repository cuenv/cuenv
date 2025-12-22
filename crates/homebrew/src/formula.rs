//! Homebrew formula generation.
//!
//! Generates Ruby formula files for Homebrew from release artifacts.

use cuenv_release::Target;
use std::collections::HashMap;

/// Binary information for a platform.
#[derive(Debug, Clone)]
pub struct BinaryInfo {
    /// Download URL
    pub url: String,
    /// SHA256 checksum
    pub sha256: String,
}

/// Data for generating a Homebrew formula.
#[derive(Debug, Clone)]
pub struct FormulaData {
    /// Formula class name (e.g., "Cuenv")
    pub class_name: String,
    /// Description
    pub desc: String,
    /// Homepage URL
    pub homepage: String,
    /// License identifier
    pub license: String,
    /// Version
    pub version: String,
    /// Binary info per target
    pub binaries: HashMap<Target, BinaryInfo>,
}

/// Homebrew formula generator.
pub struct FormulaGenerator;

impl FormulaGenerator {
    /// Generates a Ruby formula from the data.
    #[must_use]
    #[allow(clippy::format_push_string)]
    pub fn generate(data: &FormulaData) -> String {
        let mut formula = format!(
            r#"class {} < Formula
  desc "{}"
  homepage "{}"
  version "{}"
  license "{}"
"#,
            data.class_name, data.desc, data.homepage, data.version, data.license
        );

        // macOS section (Apple Silicon only)
        formula.push_str("\n  on_macos do\n");
        if let Some(info) = data.binaries.get(&Target::DarwinArm64) {
            formula.push_str(&format!(
                r#"    on_arm do
      url "{}"
      sha256 "{}"
    end
"#,
                info.url, info.sha256
            ));
        }
        formula.push_str("  end\n");

        // Linux section
        formula.push_str("\n  on_linux do\n");
        if let Some(info) = data.binaries.get(&Target::LinuxArm64) {
            formula.push_str(&format!(
                r#"    on_arm do
      url "{}"
      sha256 "{}"
    end
"#,
                info.url, info.sha256
            ));
        }
        if let Some(info) = data.binaries.get(&Target::LinuxX64) {
            formula.push_str(&format!(
                r#"    on_intel do
      url "{}"
      sha256 "{}"
    end
"#,
                info.url, info.sha256
            ));
        }
        formula.push_str("  end\n");

        // Install and test sections
        let binary_name = data.class_name.to_lowercase();
        formula.push_str("\n  def install\n");
        formula.push_str(&format!("    bin.install \"{binary_name}\"\n"));
        formula.push_str("  end\n\n");
        formula.push_str("  test do\n");
        // Ruby string interpolation: #{bin} - we need literal #{ in the output
        formula.push_str(&format!(
            "    assert_match version.to_s, shell_output(\"#{{bin}}/{binary_name} --version\")\n"
        ));
        formula.push_str("  end\nend\n");

        formula
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_formula() {
        let mut binaries = HashMap::new();
        binaries.insert(
            Target::DarwinArm64,
            BinaryInfo {
                url: "https://example.com/darwin-arm64.tar.gz".to_string(),
                sha256: "abc123".to_string(),
            },
        );
        binaries.insert(
            Target::LinuxX64,
            BinaryInfo {
                url: "https://example.com/linux-x64.tar.gz".to_string(),
                sha256: "def456".to_string(),
            },
        );

        let data = FormulaData {
            class_name: "Cuenv".to_string(),
            desc: "Test description".to_string(),
            homepage: "https://github.com/cuenv/cuenv".to_string(),
            license: "AGPL-3.0-or-later".to_string(),
            version: "0.16.0".to_string(),
            binaries,
        };

        let formula = FormulaGenerator::generate(&data);

        assert!(formula.contains("class Cuenv < Formula"));
        assert!(formula.contains("version \"0.16.0\""));
        assert!(formula.contains("on_macos do"));
        assert!(formula.contains("on_linux do"));
        assert!(formula.contains("sha256 \"abc123\""));
    }
}
