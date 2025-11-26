package schema

// Base secret type with exec resolver
#Secret: {
	resolver: "exec"
	command:  string
	args?: [...string]
	...
}

// For backward compatibility and structured types
#ExecResolver: {
	command: string
	args?: [...string]
}
