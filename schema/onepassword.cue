package schema

// 1Password secret reference
#OnePasswordRef: #Secret & {
	resolver: "onepassword"
	ref:      string // e.g., "op://vault/item/field"
}
