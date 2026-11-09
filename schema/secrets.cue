package schema

// Base secret type with resolver
// Mode is auto-negotiated based on environment variables:
// - AWS: AWS CLI standard credential and region chain
// - GCP: HTTPS API; token from GOOGLE_OAUTH_ACCESS_TOKEN, otherwise gcloud ADC
// - 1Password: OP_SERVICE_ACCOUNT_TOKEN → HTTP, otherwise CLI
// - Vault: schema only; no resolver is registered yet
// - Infisical: Universal Auth, OIDC Auth (identity ID + JWT) or INFISICAL_TOKEN → HTTP
//
// `value` and `policies` are reserved for #EnvironmentVariableWithPolicies,
// and `cuenvPassthrough` for #EnvPassthrough. Forbidding them here keeps
// `{value: ..., policies: [...]}` and `{cuenvPassthrough: true}` from also
// matching this open type, which would make the #EnvironmentVariable
// disjunction ambiguous and leave the variable unresolved.
#Secret: {
	resolver: "aws" | "gcp" | "onepassword" | "vault" | "infisical" | "exec"
	value?:            _|_
	policies?:         _|_
	cuenvPassthrough?: _|_
	...
}

// Exec resolver (legacy/custom commands)
#ExecSecret: #Secret & {
	resolver: "exec"
	command:  string
	args?: [...string]
}
