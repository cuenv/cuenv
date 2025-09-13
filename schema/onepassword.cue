package schema

#OnePasswordRef: #Secret & {
	ref: string
	resolver: "exec"
	command: "op"
	args: [
		"read",
		ref,
	]
}
