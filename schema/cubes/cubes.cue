package cubes

// ============================================================================
// Code File Schemas - Language-specific file definitions
// ============================================================================

// #CodeFile is the base schema for all generated file content
#CodeFile: {
	// The actual code content
	content: string

	// Language identifier (for syntax highlighting and formatting)
	language: string

	// File generation mode
	// - managed: Always regenerated when cube is synced
	// - scaffold: Only created if file doesn't exist
	mode: "managed" | "scaffold" | *"managed"

	// Formatting configuration
	format?: {
		indent:         "space" | "tab"
		indentSize?:    int & >=1 & <=8
		lineWidth?:     int & >=60 & <=200
		trailingComma?: "none" | "all" | "es5"
		semicolons?:    bool
		quotes?:        "single" | "double"
	}

	// Optional: Validation/linting rules
	lint?: {
		enabled: bool
		rules?: {...}
	}
}

// ============================================================================
// Language-Specific File Schemas
// ============================================================================

// TypeScript files
#TypeScriptFile: #CodeFile & {
	language: "typescript"

	format: {
		indent:        "space" | "tab" | *"space"
		indentSize:    int | *2
		lineWidth:     int | *100
		trailingComma: "none" | "all" | "es5" | *"all"
		semicolons:    bool | *true
		quotes:        "single" | "double" | *"double"
	}

	// TypeScript-specific config
	tsconfig?: {
		target?:           "ES2020" | "ES2021" | "ES2022" | *"ES2022"
		module?:           "CommonJS" | "ESNext" | "NodeNext" | *"NodeNext"
		strict?:           bool | *true
		moduleResolution?: "node" | "bundler" | *"bundler"
	}
}

// JavaScript files
#JavaScriptFile: #CodeFile & {
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
#JSONFile: #CodeFile & {
	language: "json"

	format: {
		indent:     "space" | "tab" | *"space"
		indentSize: int | *2
	}
}

// JSONC (JSON with comments) - for tsconfig.json, wrangler.jsonc, etc.
#JSONCFile: #CodeFile & {
	language: "jsonc"

	format: {
		indent:     "space" | "tab" | *"space"
		indentSize: int | *2
	}
}

// YAML files
#YAMLFile: #CodeFile & {
	language: "yaml"

	format: {
		indent:     "space" | *"space"
		indentSize: int | *2
	}
}

// TOML files
#TOMLFile: #CodeFile & {
	language: "toml"

	format: {
		indent:     "space" | *"space"
		indentSize: int | *2
	}
}

// Rust files
#RustFile: #CodeFile & {
	language: "rust"

	format: {
		indent:     "space" | *"space"
		indentSize: int | *4
		lineWidth:  int | *100
	}

	// Rust-specific config
	rustfmt?: {
		edition?:              "2018" | "2021" | *"2021"
		use_small_heuristics?: "Default" | "Off" | "Max" | *"Default"
	}
}

// Go files
#GoFile: #CodeFile & {
	language: "go"

	format: {
		indent:     "tab" | *"tab"
		indentSize: int | *8
	}
}

// Python files
#PythonFile: #CodeFile & {
	language: "python"

	format: {
		indent:     "space" | *"space"
		indentSize: int | *4
		lineWidth:  int | *88 // Black default
	}
}

// Markdown files
#MarkdownFile: #CodeFile & {
	language: "markdown"

	format: {
		indent:     "space" | *"space"
		indentSize: int | *2
		lineWidth:  int | *80
	}
}

// Shell script files
#ShellScriptFile: #CodeFile & {
	language: "shell"

	format: {
		indent:     "space" | *"space"
		indentSize: int | *2
	}
}

// Dockerfile
#DockerfileFile: #CodeFile & {
	language: "dockerfile"

	format: {
		indent:     "space" | *"space"
		indentSize: int | *4
	}
}

// Nix expressions
#NixFile: #CodeFile & {
	language: "nix"

	format: {
		indent:     "space" | *"space"
		indentSize: int | *2
	}
}
