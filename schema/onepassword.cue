package schema

#OnePasswordRef: close({
	ref:      string
	resolver: "exec"
	command:  "op"
	args: ["read", ref]
})
