package main

import "cuenv.dev/schema"

// Example IaC configuration for AWS infrastructure
iac: schema.#IaC & {
	// Provider configuration
	providers: {
		aws: {
			source:  "hashicorp/aws"
			version: "~> 5.0"
			config: {
				region: "us-east-1"
			}
		}
	}

	// Variables for parameterization
	variables: {
		environment: {
			type:        "string"
			default:     "dev"
			description: "Environment name (dev, staging, prod)"
		}
		instance_type: {
			type:        "string"
			default:     "t3.micro"
			description: "EC2 instance type"
		}
	}

	// Data sources for AMI lookup
	data: {
		"ubuntu_ami": {
			type: "aws_ami"
			config: {
				most_recent: true
				owners: ["099720109477"] // Canonical
				filter: [
					{
						name: "name"
						values: ["ubuntu/images/hvm-ssd/ubuntu-jammy-22.04-amd64-server-*"]
					},
				]
			}
		}
	}

	// Resource definitions
	resources: {
		// VPC
		"main_vpc": schema.#AWSVPC & {
			config: {
				cidr_block:           "10.0.0.0/16"
				enable_dns_hostnames: true
				enable_dns_support:   true
				tags: {
					Name:        "main-vpc"
					Environment: "${var.environment}"
				}
			}
			critical: true // Monitor with short polling interval
		}

		// Public Subnet
		"public_subnet": schema.#AWSSubnet & {
			config: {
				vpc_id:            "${resource.main_vpc.id}"
				cidr_block:        "10.0.1.0/24"
				availability_zone: "us-east-1a"
				tags: {
					Name:        "public-subnet"
					Environment: "${var.environment}"
				}
			}
			dependsOn: [
				{ref: "main_vpc"},
			]
		}

		// Security Group
		"web_sg": schema.#AWSSecurityGroup & {
			config: {
				name:        "web-sg"
				description: "Security group for web servers"
				vpc_id:      "${resource.main_vpc.id}"

				ingress: [
					{
						from_port:   80
						to_port:     80
						protocol:    "tcp"
						cidr_blocks: ["0.0.0.0/0"]
						description: "HTTP"
					},
					{
						from_port:   443
						to_port:     443
						protocol:    "tcp"
						cidr_blocks: ["0.0.0.0/0"]
						description: "HTTPS"
					},
					{
						from_port:   22
						to_port:     22
						protocol:    "tcp"
						cidr_blocks: ["10.0.0.0/8"]
						description: "SSH from internal"
					},
				]

				egress: [
					{
						from_port:   0
						to_port:     0
						protocol:    "-1"
						cidr_blocks: ["0.0.0.0/0"]
						description: "Allow all outbound"
					},
				]

				tags: {
					Name:        "web-sg"
					Environment: "${var.environment}"
				}
			}
			dependsOn: [
				{ref: "main_vpc"},
			]
		}

		// EC2 Instance
		"web_instance": schema.#AWSInstance & {
			config: {
				ami:           "${data.aws_ami.ubuntu_ami.id}"
				instance_type: "${var.instance_type}"
				subnet_id:     "${resource.public_subnet.id}"
				vpc_security_group_ids: ["${resource.web_sg.id}"]

				tags: {
					Name:        "web-server"
					Environment: "${var.environment}"
				}
			}
			dependsOn: [
				{ref: "public_subnet"},
				{ref: "web_sg"},
			]
			lifecycle: {
				createBeforeDestroy: true
			}
		}
	}

	// Outputs
	outputs: {
		vpc_id: {
			value:       "${resource.main_vpc.id}"
			description: "The ID of the VPC"
		}
		instance_public_ip: {
			value:       "${resource.web_instance.public_ip}"
			description: "Public IP of the web instance"
		}
	}

	// Drift detection configuration
	drift: {
		enablePolling:         true
		criticalPollInterval:  "5m"
		standardPollInterval:  "1h"
		enableEvents:          false
		exclude: ["aws_cloudwatch_log_group.*"]
	}
}
