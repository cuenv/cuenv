package schema

#OnePasswordRef: #Secret & {
	ref:     string
	command: "op"
	args: ["read", ref]
}
