package schema

// Base secret type with resolver
// Mode is auto-negotiated based on environment variables:
// - AWS: AWS_ACCESS_KEY_ID + AWS_SECRET_ACCESS_KEY → HTTP, otherwise CLI
// - GCP: GOOGLE_APPLICATION_CREDENTIALS → HTTP, otherwise CLI
// - 1Password: OP_SERVICE_ACCOUNT_TOKEN → HTTP, otherwise CLI
// - Vault: VAULT_TOKEN + VAULT_ADDR → HTTP, otherwise CLI
#Secret: {
	resolver: "aws" | "gcp" | "onepassword" | "vault" | "exec"
	...
}

// Exec resolver (legacy/custom commands)
#ExecSecret: #Secret & {
	resolver: "exec"
	command:  string
	args?: [...string]
}

// For backward compatibility
#ExecResolver: {
	command: string
	args?: [...string]
}
