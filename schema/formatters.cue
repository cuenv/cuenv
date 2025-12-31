package schema

// #Formatters configures language-specific code formatters.
// Each formatter can specify file patterns and tool-specific options.
#Formatters: {
	rust?: #RustFormatter
	nix?:  #NixFormatter
	go?:   #GoFormatter
	cue?:  #CueFormatter
}

// #RustFormatter configures rustfmt for Rust files.
#RustFormatter: close({
	// Whether this formatter is enabled (default: true)
	enabled: bool | *true

	// Glob patterns for files to format (default: ["*.rs"])
	includes: [...string] | *["*.rs"]

	// Rust edition for formatting rules
	edition?: "2018" | "2021" | "2024"
})

// #NixFormatter configures Nix file formatting.
#NixFormatter: close({
	// Whether this formatter is enabled (default: true)
	enabled: bool | *true

	// Glob patterns for files to format (default: ["*.nix"])
	includes: [...string] | *["*.nix"]

	// Which Nix formatter tool to use
	tool: "nixfmt" | "alejandra" | *"nixfmt"
})

// #GoFormatter configures gofmt for Go files.
#GoFormatter: close({
	// Whether this formatter is enabled (default: true)
	enabled: bool | *true

	// Glob patterns for files to format (default: ["*.go"])
	includes: [...string] | *["*.go"]
})

// #CueFormatter configures cue fmt for CUE files.
#CueFormatter: close({
	// Whether this formatter is enabled (default: true)
	enabled: bool | *true

	// Glob patterns for files to format (default: ["*.cue"])
	includes: [...string] | *["*.cue"]
})
