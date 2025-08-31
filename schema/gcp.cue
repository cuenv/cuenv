package schema

#GcpSecret: #Secret & {
	project: string
	secret:  string
	version: string | *"latest"
	ref:     "gcp://\(project)/\(secret)/\(version)"
	resolver: #ExecResolver & {
		command: "gcloud"
		args: [
			"secrets", "versions", "access", version,
			"--secret", secret,
			"--project", project,
		]
	}
}
