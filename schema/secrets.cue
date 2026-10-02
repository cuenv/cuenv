package schema

// Base secret type with resolver
// Mode is auto-negotiated based on environment variables:
// - AWS: AWS CLI standard credential and region chain
// - GCP: GOOGLE_APPLICATION_CREDENTIALS → HTTP, otherwise CLI
// - 1Password: OP_SERVICE_ACCOUNT_TOKEN → HTTP, otherwise CLI
// - Vault: VAULT_TOKEN + VAULT_ADDR → HTTP, otherwise CLI
// - Infisical: Universal Auth env vars or INFISICAL_TOKEN → HTTP
//
// `value` and `policies` are reserved for #EnvironmentVariableWithPolicies.
// Forbidding them here keeps `{value: ..., policies: [...]}` from also
// matching this open type, which would make the #EnvironmentVariable
// disjunction ambiguous and leave the variable unresolved.
#Secret: {
	resolver: "aws" | "gcp" | "onepassword" | "vault" | "infisical" | "exec"
	value?:    _|_
	policies?: _|_
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
