package schema

#GcpSecret: close({
	project:  string
	secret:   string
	version:  string | *"latest"
	ref:      "gcp://\(project)/\(secret)/\(version)"
	resolver: "exec"
	command:  "gcloud"
	args: ["secrets", "versions", "access", version, "--secret", secret, "--project", project]
})
