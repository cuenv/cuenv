package schema

// HashiCorp Vault secret
#VaultSecret: #Secret & {
	resolver: "vault"
	path:     string        // Path to secret (e.g., "myapp/config")
	key:      string        // Key within the secret
	mount:    string | *"secret" // Secret engine mount point
}
