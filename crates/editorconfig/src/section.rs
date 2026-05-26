#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

/// A section in an EditorConfig file.
///
/// Each section corresponds to a glob pattern and contains settings for files
/// matching that pattern.
///
/// # Example
///
/// ```rust
/// use cuenv_editorconfig::EditorConfigSection;
///
/// let section = EditorConfigSection::new()
///     .indent_style("space")
///     .indent_size(4)
///     .end_of_line("lf")
///     .charset("utf-8")
///     .insert_final_newline(true)
///     .trim_trailing_whitespace(true);
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct EditorConfigSection {
    indent_style: Option<String>,
    indent_size: Option<EditorConfigValue>,
    tab_width: Option<u32>,
    end_of_line: Option<String>,
    charset: Option<String>,
    trim_trailing_whitespace: Option<bool>,
    insert_final_newline: Option<bool>,
    max_line_length: Option<EditorConfigValue>,
}

/// A value that can be either an integer or a special string value.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(untagged))]
pub enum EditorConfigValue {
    /// Integer value
    Int(u32),
    /// String value (e.g., "tab" for indent_size, "off" for max_line_length)
    String(String),
}

impl EditorConfigValue {
    pub(crate) fn to_editorconfig_value(&self) -> String {
        match self {
            Self::Int(n) => n.to_string(),
            Self::String(s) => s.clone(),
        }
    }
}

impl EditorConfigSection {
    /// Create a new empty section.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the indentation style.
    ///
    /// Valid values: "tab", "space"
    #[must_use]
    pub fn indent_style(mut self, style: impl Into<String>) -> Self {
        self.indent_style = Some(style.into());
        self
    }

    /// Set the indentation style from an optional value.
    #[must_use]
    pub fn indent_style_opt(mut self, style: Option<impl Into<String>>) -> Self {
        self.indent_style = style.map(Into::into);
        self
    }

    /// Set the indentation size.
    #[must_use]
    pub fn indent_size(mut self, size: u32) -> Self {
        self.indent_size = Some(EditorConfigValue::Int(size));
        self
    }

    /// Set the indentation size to "tab".
    #[must_use]
    pub fn indent_size_tab(mut self) -> Self {
        self.indent_size = Some(EditorConfigValue::String("tab".to_string()));
        self
    }

    /// Set the indentation size from an optional value.
    #[must_use]
    pub fn indent_size_opt(mut self, size: Option<EditorConfigValue>) -> Self {
        self.indent_size = size;
        self
    }

    /// Set the tab width.
    #[must_use]
    pub fn tab_width(mut self, width: u32) -> Self {
        self.tab_width = Some(width);
        self
    }

    /// Set the tab width from an optional value.
    #[must_use]
    pub fn tab_width_opt(mut self, width: Option<u32>) -> Self {
        self.tab_width = width;
        self
    }

    /// Set the line ending style.
    ///
    /// Valid values: "lf", "crlf", "cr"
    #[must_use]
    pub fn end_of_line(mut self, eol: impl Into<String>) -> Self {
        self.end_of_line = Some(eol.into());
        self
    }

    /// Set the line ending style from an optional value.
    #[must_use]
    pub fn end_of_line_opt(mut self, eol: Option<impl Into<String>>) -> Self {
        self.end_of_line = eol.map(Into::into);
        self
    }

    /// Set the character encoding.
    ///
    /// Valid values: "utf-8", "utf-8-bom", "utf-16be", "utf-16le", "latin1"
    #[must_use]
    pub fn charset(mut self, charset: impl Into<String>) -> Self {
        self.charset = Some(charset.into());
        self
    }

    /// Set the character encoding from an optional value.
    #[must_use]
    pub fn charset_opt(mut self, charset: Option<impl Into<String>>) -> Self {
        self.charset = charset.map(Into::into);
        self
    }

    /// Set whether to trim trailing whitespace.
    #[must_use]
    pub fn trim_trailing_whitespace(mut self, trim: bool) -> Self {
        self.trim_trailing_whitespace = Some(trim);
        self
    }

    /// Set whether to trim trailing whitespace from an optional value.
    #[must_use]
    pub fn trim_trailing_whitespace_opt(mut self, trim: Option<bool>) -> Self {
        self.trim_trailing_whitespace = trim;
        self
    }

    /// Set whether to insert a final newline.
    #[must_use]
    pub fn insert_final_newline(mut self, insert: bool) -> Self {
        self.insert_final_newline = Some(insert);
        self
    }

    /// Set whether to insert a final newline from an optional value.
    #[must_use]
    pub fn insert_final_newline_opt(mut self, insert: Option<bool>) -> Self {
        self.insert_final_newline = insert;
        self
    }

    /// Set the maximum line length.
    #[must_use]
    pub fn max_line_length(mut self, length: u32) -> Self {
        self.max_line_length = Some(EditorConfigValue::Int(length));
        self
    }

    /// Set the maximum line length to "off".
    #[must_use]
    pub fn max_line_length_off(mut self) -> Self {
        self.max_line_length = Some(EditorConfigValue::String("off".to_string()));
        self
    }

    /// Set the maximum line length from an optional value.
    #[must_use]
    pub fn max_line_length_opt(mut self, length: Option<EditorConfigValue>) -> Self {
        self.max_line_length = length;
        self
    }

    /// Check if this section has any settings.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.indent_style.is_none()
            && self.indent_size.is_none()
            && self.tab_width.is_none()
            && self.end_of_line.is_none()
            && self.charset.is_none()
            && self.trim_trailing_whitespace.is_none()
            && self.insert_final_newline.is_none()
            && self.max_line_length.is_none()
    }

    /// Generate the content for this section (without the header).
    pub(crate) fn generate_content(&self) -> Vec<String> {
        let mut lines = Vec::new();

        if let Some(ref style) = self.indent_style {
            lines.push(format!("indent_style = {style}"));
        }
        if let Some(ref size) = self.indent_size {
            lines.push(format!("indent_size = {}", size.to_editorconfig_value()));
        }
        if let Some(width) = self.tab_width {
            lines.push(format!("tab_width = {width}"));
        }
        if let Some(ref eol) = self.end_of_line {
            lines.push(format!("end_of_line = {eol}"));
        }
        if let Some(ref charset) = self.charset {
            lines.push(format!("charset = {charset}"));
        }
        if let Some(trim) = self.trim_trailing_whitespace {
            lines.push(format!("trim_trailing_whitespace = {trim}"));
        }
        if let Some(insert) = self.insert_final_newline {
            lines.push(format!("insert_final_newline = {insert}"));
        }
        if let Some(ref length) = self.max_line_length {
            lines.push(format!(
                "max_line_length = {}",
                length.to_editorconfig_value()
            ));
        }

        lines
    }
}
