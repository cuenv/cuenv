package schema

// ============================================================================
// Cube - Code Generation from CUE Templates
// ============================================================================

// #Cube defines a set of files to generate from CUE configuration.
// Add a cube field to your #Project to enable code generation.
//
// Example:
//   schema.#Project & {
//       name: "my-service"
//       cube: {
//           context: { serviceName: "users" }
//           files: {
//               "package.json": #JSON & {
//                   content: """
//                   { "name": "\(context.serviceName)" }
//                   """
//               }
//           }
//       }
//   }
#Cube: {
	// Files to generate
	// Key is the file path relative to project directory
	files: [string]: #Code

	// Optional context data for templating
	// Use this to pass configuration to your cube
	context?: _
}

// ============================================================================
// Code Schemas - Language-specific file definitions
// ============================================================================

// #Code is the base schema for all generated file content
#Code: {
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
// Language-Specific Schemas
// ============================================================================

// TypeScript files
#TypeScript: #Code & {
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

// JSON files
#JSON: #Code & {
	language: "json"

	format: {
		indent:     "space" | "tab" | *"space"
		indentSize: int | *2
	}
}

// JSONC (JSON with comments) - for tsconfig.json, wrangler.jsonc, etc.
#JSONC: #Code & {
	language: "jsonc"

	format: {
		indent:     "space" | "tab" | *"space"
		indentSize: int | *2
	}
}

// YAML files
#YAML: #Code & {
	language: "yaml"

	format: {
		indent:     "space" | *"space"
		indentSize: int | *2
	}
}

// TOML files
#TOML: #Code & {
	language: "toml"

	format: {
		indent:     "space" | *"space"
		indentSize: int | *2
	}
}

// Rust files
#Rust: #Code & {
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
#Go: #Code & {
	language: "go"

	format: {
		indent:     "tab" | *"tab"
		indentSize: int | *8
	}
}

// Python files
#Python: #Code & {
	language: "python"

	format: {
		indent:     "space" | *"space"
		indentSize: int | *4
		lineWidth:  int | *88 // Black default
	}
}

// Markdown files
#Markdown: #Code & {
	language: "markdown"

	format: {
		indent:     "space" | *"space"
		indentSize: int | *2
		lineWidth:  int | *80
	}
}

// Shell script files
#ShellScript: #Code & {
	language: "shell"

	format: {
		indent:     "space" | *"space"
		indentSize: int | *2
	}
}

// Dockerfile
#Dockerfile: #Code & {
	language: "dockerfile"

	format: {
		indent:     "space" | *"space"
		indentSize: int | *4
	}
}

// Nix expressions
#Nix: #Code & {
	language: "nix"

	format: {
		indent:     "space" | *"space"
		indentSize: int | *2
	}
}
