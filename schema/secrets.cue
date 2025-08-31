package schema

#Secret: {
	resolver: #ExecResolver
	...
}

#ExecResolver: {
	command: string
	args: [...string]
}
