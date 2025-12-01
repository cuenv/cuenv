package schema

#OnePasswordRef: close({
	resolver: "exec"
	ref:      string
	command:  "op"
	args: ["read", ref]
})
