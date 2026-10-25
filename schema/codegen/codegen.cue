package codegen

// ============================================================================
// Project File Schemas - Language-specific file definitions
// ============================================================================

// #ProjectFile is the base schema for all generated file content
#ProjectFile: {
	// The actual code content
	content: string

	// Language identifier (for syntax highlighting and formatting)
	language: string

	// File generation mode
	// - managed: Always regenerated when cube is synced
	// - scaffold: Only created if file doesn't exist
	mode: "managed" | "scaffold" | *"managed"

	// Whether to add this file path to .gitignore
	// Defaults based on mode:
	//   - managed: defaults to true (generated files should be ignored)
	//   - scaffold: defaults to false (user-owned files should be committed)
	gitignore: bool
	if mode == "managed" {
		gitignore: *true | bool
	}
	if mode == "scaffold" {
		gitignore: *false | bool
	}

	// Formatting configuration
	format?: {
		indent:         "space" | "tab"
		indentSize?:    int & >=1 & <=8
		lineWidth?:     int & >=60 & <=200
		trailingComma?: "none" | "all" | "es5"
		semicolons?:    bool
		quotes?:        "single" | "double"
	}

	// Optional: validation/linting. When enabled, generated content is
	// validated during `cuenv sync codegen` (JSON syntax today; other
	// languages are accepted as a no-op until validators are added).
	lint?: {
		enabled: bool
	}
}

// ============================================================================
// Language-Specific File Schemas
// ============================================================================

// TypeScript files
#TypeScriptFile: #ProjectFile & {
	language: "typescript"

	format: {
		indent:        "space" | "tab" | *"space"
		indentSize:    int | *2
		lineWidth:     int | *100
		trailingComma: "none" | "all" | "es5" | *"all"
		semicolons:    bool | *true
		quotes:        "single" | "double" | *"double"
	}
}

// JavaScript files
#JavaScriptFile: #ProjectFile & {
	language: "javascript"

	format: {
		indent:        "space" | "tab" | *"space"
		indentSize:    int | *2
		lineWidth:     int | *100
		trailingComma: "none" | "all" | "es5" | *"all"
		semicolons:    bool | *true
		quotes:        "single" | "double" | *"double"
	}
}

// JSON files
#JSONFile: #ProjectFile & {
	language: "json"

	format: {
		indent:     "space" | "tab" | *"space"
		indentSize: int | *2
	}
}

// JSONC (JSON with comments) - for tsconfig.json, wrangler.jsonc, etc.
#JSONCFile: #ProjectFile & {
	language: "jsonc"

	format: {
		indent:     "space" | "tab" | *"space"
		indentSize: int | *2
	}
}

// YAML files
#YAMLFile: #ProjectFile & {
	language: "yaml"

	format: {
		indent:     "space" | *"space"
		indentSize: int | *2
	}
}

// TOML files
#TOMLFile: #ProjectFile & {
	language: "toml"

	format: {
		indent:     "space" | *"space"
		indentSize: int | *2
	}
}

// Rust files
#RustFile: #ProjectFile & {
	language: "rust"

	format: {
		indent:     "space" | *"space"
		indentSize: int | *4
		lineWidth:  int | *100
	}
}

// Go files
#GoFile: #ProjectFile & {
	language: "go"

	format: {
		indent:     "tab" | *"tab"
		indentSize: int | *8
	}
}

// Python files
#PythonFile: #ProjectFile & {
	language: "python"

	format: {
		indent:     "space" | *"space"
		indentSize: int | *4
		lineWidth:  int | *88 // Black default
	}
}

// Markdown files
#MarkdownFile: #ProjectFile & {
	language: "markdown"

	format: {
		indent:     "space" | *"space"
		indentSize: int | *2
		lineWidth:  int | *80
	}
}

// Shell script files
#ShellScriptFile: #ProjectFile & {
	language: "shell"

	format: {
		indent:     "space" | *"space"
		indentSize: int | *2
	}
}

// Dockerfile
#DockerfileFile: #ProjectFile & {
	language: "dockerfile"

	format: {
		indent:     "space" | *"space"
		indentSize: int | *4
	}
}

// Nix expressions
#NixFile: #ProjectFile & {
	language: "nix"

	format: {
		indent:     "space" | *"space"
		indentSize: int | *2
	}
}
