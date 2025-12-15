// Base schemas for code generation
package code

// Base schema for all code content
#Code: {
	// The actual code content
	content: string

	// Language identifier (for syntax highlighting)
	language: string

	// Formatting configuration
	format?: {
		indent:        "space" | "tab"
		indentSize?:   int & >=1 & <=8
		lineWidth?:    int & >=60 & <=200
		trailingComma?: "none" | "all" | "es5"
		semicolons?:   bool
		quotes?:       "single" | "double"
	}

	// File generation mode
	mode: "managed" | "scaffold" | *"managed"

	// Optional: Validation/linting rules
	lint?: {
		enabled: bool
		rules?: {...}
	}
}

// TypeScript-specific schema
#TypeScript: #Code & {
	language: "typescript"

	// TypeScript-specific formatting defaults
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
		target?:          "ES2020" | "ES2021" | "ES2022" | *"ES2022"
		module?:          "CommonJS" | "ESNext" | "NodeNext" | *"NodeNext"
		strict?:          bool | *true
		moduleResolution?: "node" | "bundler" | *"bundler"
	}
}

// JavaScript-specific schema
#JavaScript: #Code & {
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

// Rust-specific schema
#Rust: #Code & {
	language: "rust"

	format: {
		indent:     "space" | *"space" // Rust convention
		indentSize: int | *4
		lineWidth:  int | *100
	}

	// Rust-specific config
	rustfmt?: {
		edition?:             "2018" | "2021" | *"2021"
		use_small_heuristics?: "Default" | "Off" | "Max" | *"Default"
	}
}

// JSON-specific schema
#JSON: #Code & {
	language: "json"

	format: {
		indent:     "space" | "tab" | *"space"
		indentSize: int | *2
	}
}

// YAML-specific schema
#YAML: #Code & {
	language: "yaml"

	format: {
		indent:     "space" | *"space"
		indentSize: int | *2
	}
}

// TOML-specific schema
#TOML: #Code & {
	language: "toml"

	format: {
		indent:     "space" | *"space"
		indentSize: int | *2
	}
}

// Go-specific schema
#Go: #Code & {
	language: "go"

	format: {
		indent:     "tab" | *"tab" // Go convention
		indentSize: int | *8
	}
}

// Python-specific schema
#Python: #Code & {
	language: "python"

	format: {
		indent:     "space" | *"space" // PEP 8 convention
		indentSize: int | *4
		lineWidth:  int | *88 // Black default
	}
}

// Markdown-specific schema
#Markdown: #Code & {
	language: "markdown"

	format: {
		indent:     "space" | *"space"
		indentSize: int | *2
		lineWidth:  int | *80
	}
}
