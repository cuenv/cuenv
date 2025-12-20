package schema

// AWS Secrets Manager secret
#AwsSecret: #Secret & {
	resolver: "aws"
	secretId: string // ARN or secret name

	// Optional version specifiers
	versionId?:    string
	versionStage?: string

	// Extract a specific field from JSON secrets
	jsonKey?: string
}
