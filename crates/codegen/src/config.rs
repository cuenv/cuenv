//! Formatter configuration generation
//!
//! This module handles automatic generation of formatter configuration files
//! (biome.json, .prettierrc, rustfmt.toml, etc.) based on CUE schema format settings.

use crate::cube::FormatConfig;
use serde_json::json;

/// Generate biome.json configuration from format config
#[must_use]
pub fn generate_biome_config(format: &FormatConfig) -> serde_json::Value {
    json!({
        "$schema": "https://biomejs.dev/schemas/1.4.1/schema.json",
        "formatter": {
            "enabled": true,
            "indentStyle": format.indent,
            "indentSize": format.indent_size.unwrap_or(2),
            "lineWidth": format.line_width.unwrap_or(100)
        },
        "linter": {
            "enabled": true
        },
        "javascript": {
            "formatter": {
                "quoteStyle": format.quotes.as_deref().unwrap_or("double"),
                "trailingComma": format.trailing_comma.as_deref().unwrap_or("all"),
                "semicolons": if format.semicolons.unwrap_or(true) { "always" } else { "asNeeded" }
            }
        }
    })
}

/// Generate .prettierrc configuration from format config
#[must_use]
pub fn generate_prettier_config(format: &FormatConfig) -> serde_json::Value {
    json!({
        "useTabs": format.indent == "tab",
        "tabWidth": format.indent_size.unwrap_or(2),
        "printWidth": format.line_width.unwrap_or(100),
        "singleQuote": format.quotes.as_deref() == Some("single"),
        "trailingComma": format.trailing_comma.as_deref().unwrap_or("all"),
        "semi": format.semicolons.unwrap_or(true)
    })
}

/// Generate rustfmt.toml configuration from format config
#[must_use]
pub fn generate_rustfmt_config(format: &FormatConfig) -> String {
    let edition = "2021"; // Default to 2021 edition
    let max_width = format.line_width.unwrap_or(100);
    let hard_tabs = format.indent == "tab";
    let tab_spaces = format.indent_size.unwrap_or(4);

    format!(
        r#"edition = "{edition}"
max_width = {max_width}
hard_tabs = {hard_tabs}
tab_spaces = {tab_spaces}
use_small_heuristics = "Default"
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_biome_config() {
        let format = FormatConfig {
            indent: "space".to_string(),
            indent_size: Some(2),
            line_width: Some(100),
            quotes: Some("single".to_string()),
            semicolons: Some(false),
            trailing_comma: Some("es5".to_string()),
        };

        let config = generate_biome_config(&format);
        assert_eq!(config["formatter"]["indentStyle"], "space");
        assert_eq!(config["formatter"]["indentSize"], 2);
        assert_eq!(config["javascript"]["formatter"]["quoteStyle"], "single");
    }

    #[test]
    fn test_generate_prettier_config() {
        let format = FormatConfig {
            indent: "tab".to_string(),
            indent_size: Some(4),
            line_width: Some(120),
            quotes: Some("single".to_string()),
            semicolons: Some(false),
            trailing_comma: Some("none".to_string()),
        };

        let config = generate_prettier_config(&format);
        assert_eq!(config["useTabs"], true);
        assert_eq!(config["tabWidth"], 4);
        assert_eq!(config["singleQuote"], true);
        assert_eq!(config["semi"], false);
    }

    #[test]
    fn test_generate_rustfmt_config() {
        let format = FormatConfig {
            indent: "space".to_string(),
            indent_size: Some(4),
            line_width: Some(100),
            ..Default::default()
        };

        let config = generate_rustfmt_config(&format);
        assert!(config.contains("edition = \"2021\""));
        assert!(config.contains("max_width = 100"));
        assert!(config.contains("hard_tabs = false"));
        assert!(config.contains("tab_spaces = 4"));
    }
}
