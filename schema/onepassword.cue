package schema

#OnePasswordRef: #Secret & {
	ref: string
	resolver: #ExecResolver & {
		command: "op"
		args: [
			"read",
			ref,
		]
	}
}
