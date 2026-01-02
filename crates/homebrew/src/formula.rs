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
    #[allow(clippy::format_push_string, clippy::too_many_lines)]
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

    #[test]
    fn test_binary_info_clone() {
        let info = BinaryInfo {
            url: "https://example.com/binary.tar.gz".to_string(),
            sha256: "abc123def456".to_string(),
        };
        let cloned = info.clone();
        assert_eq!(info.url, cloned.url);
        assert_eq!(info.sha256, cloned.sha256);
    }

    #[test]
    fn test_binary_info_debug() {
        let info = BinaryInfo {
            url: "https://example.com/binary.tar.gz".to_string(),
            sha256: "abc123".to_string(),
        };
        let debug_str = format!("{info:?}");
        assert!(debug_str.contains("BinaryInfo"));
        assert!(debug_str.contains("https://example.com"));
        assert!(debug_str.contains("abc123"));
    }

    #[test]
    fn test_formula_data_clone() {
        let data = FormulaData {
            class_name: "Test".to_string(),
            desc: "Test desc".to_string(),
            homepage: "https://test.com".to_string(),
            license: "MIT".to_string(),
            version: "1.0.0".to_string(),
            binaries: HashMap::new(),
        };
        let cloned = data.clone();
        assert_eq!(data.class_name, cloned.class_name);
        assert_eq!(data.version, cloned.version);
    }

    #[test]
    fn test_formula_data_debug() {
        let data = FormulaData {
            class_name: "Myapp".to_string(),
            desc: "My app".to_string(),
            homepage: "https://myapp.com".to_string(),
            license: "Apache-2.0".to_string(),
            version: "2.1.0".to_string(),
            binaries: HashMap::new(),
        };
        let debug_str = format!("{data:?}");
        assert!(debug_str.contains("FormulaData"));
        assert!(debug_str.contains("Myapp"));
    }

    #[test]
    fn test_generate_formula_linux_arm64() {
        let mut binaries = HashMap::new();
        binaries.insert(
            Target::LinuxArm64,
            BinaryInfo {
                url: "https://example.com/linux-arm64.tar.gz".to_string(),
                sha256: "arm64hash".to_string(),
            },
        );

        let data = FormulaData {
            class_name: "Test".to_string(),
            desc: "Test".to_string(),
            homepage: "https://test.com".to_string(),
            license: "MIT".to_string(),
            version: "1.0.0".to_string(),
            binaries,
        };

        let formula = FormulaGenerator::generate(&data);
        assert!(formula.contains("on_arm do"));
        assert!(formula.contains("arm64hash"));
    }

    #[test]
    fn test_generate_formula_all_platforms() {
        let mut binaries = HashMap::new();
        binaries.insert(
            Target::DarwinArm64,
            BinaryInfo {
                url: "https://example.com/darwin-arm64.tar.gz".to_string(),
                sha256: "darwin_arm_hash".to_string(),
            },
        );
        binaries.insert(
            Target::LinuxArm64,
            BinaryInfo {
                url: "https://example.com/linux-arm64.tar.gz".to_string(),
                sha256: "linux_arm_hash".to_string(),
            },
        );
        binaries.insert(
            Target::LinuxX64,
            BinaryInfo {
                url: "https://example.com/linux-x64.tar.gz".to_string(),
                sha256: "linux_x64_hash".to_string(),
            },
        );

        let data = FormulaData {
            class_name: "Multiplatform".to_string(),
            desc: "Multi-platform app".to_string(),
            homepage: "https://example.com".to_string(),
            license: "BSD-3-Clause".to_string(),
            version: "3.2.1".to_string(),
            binaries,
        };

        let formula = FormulaGenerator::generate(&data);
        assert!(formula.contains("darwin_arm_hash"));
        assert!(formula.contains("linux_arm_hash"));
        assert!(formula.contains("linux_x64_hash"));
        assert!(formula.contains("on_intel do"));
    }

    #[test]
    fn test_generate_formula_install_section() {
        let data = FormulaData {
            class_name: "Myapp".to_string(),
            desc: "desc".to_string(),
            homepage: "https://x.com".to_string(),
            license: "MIT".to_string(),
            version: "1.0.0".to_string(),
            binaries: HashMap::new(),
        };

        let formula = FormulaGenerator::generate(&data);
        assert!(formula.contains("def install"));
        assert!(formula.contains("bin.install \"myapp\""));
    }

    #[test]
    fn test_generate_formula_test_section() {
        let data = FormulaData {
            class_name: "Cuenv".to_string(),
            desc: "desc".to_string(),
            homepage: "https://x.com".to_string(),
            license: "MIT".to_string(),
            version: "1.0.0".to_string(),
            binaries: HashMap::new(),
        };

        let formula = FormulaGenerator::generate(&data);
        assert!(formula.contains("test do"));
        assert!(formula.contains("assert_match version.to_s"));
        assert!(formula.contains("cuenv --version"));
    }

    #[test]
    fn test_generate_formula_empty_binaries() {
        let data = FormulaData {
            class_name: "Empty".to_string(),
            desc: "No binaries".to_string(),
            homepage: "https://empty.com".to_string(),
            license: "GPL-3.0".to_string(),
            version: "0.0.1".to_string(),
            binaries: HashMap::new(),
        };

        let formula = FormulaGenerator::generate(&data);
        // Should still generate valid structure even with no binaries
        assert!(formula.contains("class Empty < Formula"));
        assert!(formula.contains("on_macos do"));
        assert!(formula.contains("on_linux do"));
        assert!(formula.ends_with("end\n"));
    }

    #[test]
    fn test_formula_special_characters_in_desc() {
        let data = FormulaData {
            class_name: "Test".to_string(),
            desc: "App with special chars: &, <, >, quotes".to_string(),
            homepage: "https://test.com".to_string(),
            license: "MIT".to_string(),
            version: "1.0.0".to_string(),
            binaries: HashMap::new(),
        };

        let formula = FormulaGenerator::generate(&data);
        assert!(formula.contains("App with special chars"));
    }
}
