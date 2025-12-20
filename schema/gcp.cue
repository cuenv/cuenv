package schema

// GCP Secret Manager secret
#GcpSecret: #Secret & {
	resolver: "gcp"
	project:  string
	secret:   string
	version:  string | *"latest"

	// Computed fields for reference
	_ref: "projects/\(project)/secrets/\(secret)/versions/\(version)"
}
