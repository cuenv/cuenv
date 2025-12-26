package rules

// #EditorConfig configuration for .editorconfig generation
// Note: `root = true` is auto-injected for the .editorconfig at repo root
#EditorConfig: {
	// File-pattern specific settings
	// Patterns use EditorConfig glob syntax (e.g., "*", "*.md", "Makefile")
	[pattern=string]: #EditorConfigSection
}

#EditorConfigSection: {
	// Indentation style: "tab" or "space"
	indent_style?: "tab" | "space"

	// Number of columns for each indentation level, or "tab" to use tab_width
	indent_size?: int | "tab"

	// Number of columns for tab character display
	tab_width?: int

	// Line ending style
	end_of_line?: "lf" | "crlf" | "cr"

	// Character encoding
	charset?: "utf-8" | "utf-8-bom" | "utf-16be" | "utf-16le" | "latin1"

	// Remove trailing whitespace on save
	trim_trailing_whitespace?: bool

	// Ensure file ends with a newline
	insert_final_newline?: bool

	// Maximum line length (soft limit), or "off" to disable
	max_line_length?: int | "off"
}
