package schema

// Infrastructure as Code resource definitions
// Enables CUE-powered infrastructure management with Terraform providers

// #IaC is the root configuration for infrastructure as code
#IaC: close({
	// Provider configurations
	providers?: [string]: #Provider

	// Resource definitions
	resources?: [string]: #Resource

	// Data source definitions (read-only)
	data?: [string]: #DataSource

	// Variable definitions for parameterization
	variables?: [string]: #Variable

	// Output definitions for exposing values
	outputs?: [string]: #Output

	// Backend configuration for state storage
	backend?: #Backend

	// Drift detection configuration
	drift?: #DriftConfig
})

// #Provider configures a Terraform provider
#Provider: close({
	// Provider source (e.g., "hashicorp/aws", "hashicorp/google")
	source!: =~"^[a-z0-9-]+/[a-z0-9-]+$"

	// Version constraint (e.g., "~> 5.0", ">= 4.0, < 6.0")
	version?: string

	// Provider-specific configuration
	config?: _

	// Alias for multiple provider configurations
	// Example: alias: "us-west" for a second AWS provider in us-west-2
	alias?: string
})

// #Resource defines a managed infrastructure resource
#Resource: close({
	// Resource type (e.g., "aws_instance", "google_compute_instance")
	type!: =~"^[a-z]+_[a-z0-9_]+$"

	// Provider to use (defaults to provider inferred from type prefix)
	provider?: string

	// Resource configuration - schema depends on resource type
	config!: _

	// Explicit dependencies on other resources
	dependsOn?: [...#ResourceRef]

	// Resource lifecycle configuration
	lifecycle?: #Lifecycle

	// Provisioners to run on the resource
	provisioners?: [...#Provisioner]

	// Create multiple instances with count
	count?: int & >0

	// Create instances from a map (mutually exclusive with count)
	forEach?: _

	// Mark as critical for drift detection (5-15 min polling)
	critical?: bool | *false
})

// #DataSource defines a read-only data source
#DataSource: close({
	// Data source type (e.g., "aws_ami", "google_compute_image")
	type!: =~"^[a-z]+_[a-z0-9_]+$"

	// Provider to use
	provider?: string

	// Filter configuration for the data source
	config!: _

	// Dependencies (data sources can depend on resources)
	dependsOn?: [...#ResourceRef]
})

// #ResourceRef references another resource or data source
#ResourceRef: close({
	// Reference format: "resource.name" or "data.type.name"
	ref!: =~"^(resource|data)\\.[a-z0-9_]+\\.[a-z0-9_]+$" | =~"^[a-z0-9_]+$"

	// Optional attribute path
	attribute?: string
})

// #Variable defines an input variable
#Variable: close({
	// Variable type (CUE type expression)
	type?: "string" | "number" | "bool" | "list" | "map" | "object" | *"string"

	// Default value
	default?: _

	// Human-readable description
	description?: string

	// Validation rules
	validation?: [...#ValidationRule]

	// Mark as sensitive (won't be displayed in logs)
	sensitive?: bool | *false

	// Whether null is allowed
	nullable?: bool | *true
})

// #ValidationRule defines a validation constraint
#ValidationRule: close({
	// Condition expression (CUE constraint or Terraform-style condition)
	condition!: string

	// Error message when validation fails
	errorMessage!: string
})

// #Output defines an output value
#Output: close({
	// Value expression
	value!: _

	// Human-readable description
	description?: string

	// Mark as sensitive
	sensitive?: bool | *false

	// Dependencies for output ordering
	dependsOn?: [...#ResourceRef]
})

// #Lifecycle configures resource lifecycle behavior
#Lifecycle: close({
	// Create new resource before destroying old one
	createBeforeDestroy?: bool | *false

	// Prevent destruction of the resource
	preventDestroy?: bool | *false

	// Ignore changes to specified attributes
	// Use ["all"] to ignore all changes
	ignoreChanges?: [...string]

	// Trigger replacement when specified expressions change
	replaceTriggeredBy?: [...string]
})

// #Provisioner configures post-creation provisioning
#Provisioner: close({
	// Provisioner type
	type!: "local-exec" | "remote-exec" | "file"

	// Provisioner-specific configuration
	config!: _

	// When to run the provisioner
	when?: "create" | "destroy" | *"create"

	// How to handle failure
	onFailure?: "fail" | "continue" | *"fail"
})

// #Backend configures state storage backend
#Backend: close({
	// Backend type (e.g., "s3", "gcs", "azurerm", "local")
	type!: string

	// Backend-specific configuration
	config?: _
})

// #DriftConfig configures drift detection
#DriftConfig: close({
	// Enable polling-based drift detection
	enablePolling?: bool | *true

	// Polling interval for critical resources (Go duration string)
	criticalPollInterval?: =~"^[0-9]+(s|m|h)$" | *"5m"

	// Polling interval for standard resources
	standardPollInterval?: =~"^[0-9]+(s|m|h)$" | *"1h"

	// Enable event-based drift detection (requires cloud integration)
	enableEvents?: bool | *false

	// Resources to exclude from drift detection
	exclude?: [...string]
})

// AWS-specific resource configurations
#AWSResource: #Resource & {
	type: =~"^aws_"
}

// Common AWS resource patterns
#AWSInstance: #AWSResource & {
	type: "aws_instance"
	config: {
		ami!:           string
		instance_type!: string
		subnet_id?:     string
		vpc_security_group_ids?: [...string]
		tags?: [string]: string
		...
	}
}

#AWSVPC: #AWSResource & {
	type: "aws_vpc"
	config: {
		cidr_block!:          =~"^[0-9]+\\.[0-9]+\\.[0-9]+\\.[0-9]+/[0-9]+$"
		enable_dns_hostnames?: bool
		enable_dns_support?:   bool
		tags?: [string]: string
		...
	}
}

#AWSSubnet: #AWSResource & {
	type: "aws_subnet"
	config: {
		vpc_id!:            string
		cidr_block!:        =~"^[0-9]+\\.[0-9]+\\.[0-9]+\\.[0-9]+/[0-9]+$"
		availability_zone?: string
		tags?: [string]: string
		...
	}
}

#AWSSecurityGroup: #AWSResource & {
	type: "aws_security_group"
	config: {
		name?:        string
		description?: string
		vpc_id?:      string
		ingress?: [...#SecurityGroupRule]
		egress?: [...#SecurityGroupRule]
		tags?: [string]: string
		...
	}
}

#SecurityGroupRule: {
	from_port!:   int
	to_port!:     int
	protocol!:    string
	cidr_blocks?: [...string]
	description?: string
	...
}

// GCP-specific resource configurations
#GCPResource: #Resource & {
	type: =~"^google_"
}

#GCPComputeInstance: #GCPResource & {
	type: "google_compute_instance"
	config: {
		name!:         string
		machine_type!: string
		zone!:         string
		boot_disk!: {
			initialize_params: {
				image!: string
				...
			}
			...
		}
		network_interface!: [...{
			network!: string
			...
		}]
		...
	}
}

// Azure-specific resource configurations
#AzureResource: #Resource & {
	type: =~"^azurerm_"
}

#AzureResourceGroup: #AzureResource & {
	type: "azurerm_resource_group"
	config: {
		name!:     string
		location!: string
		tags?: [string]: string
		...
	}
}

// Expression helpers for referencing resources
#Ref: {
	// Reference a resource attribute
	// Usage: #Ref & { resource: "vpc", attribute: "id" }
	resource!:  string
	attribute!: string
	_expr:      "${resource.\(resource).\(attribute)}"
}

#DataRef: {
	// Reference a data source attribute
	// Usage: #DataRef & { data: "aws_ami", name: "ubuntu", attribute: "id" }
	dataType!:  string
	name!:      string
	attribute!: string
	_expr:      "${data.\(dataType).\(name).\(attribute)}"
}

#VarRef: {
	// Reference a variable
	// Usage: #VarRef & { variable: "environment" }
	variable!: string
	_expr:     "${var.\(variable)}"
}
