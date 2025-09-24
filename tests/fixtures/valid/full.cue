package cuenv

import "github.com/cuenv/cuenv/schema"

// Full configuration with all features
schema.#Cuenv & {
	config: {
		outputFormat: "json"
	}
	
	env: {
		DATABASE_URL: "postgres://localhost/mydb"
		API_KEY: {
			resolver: "exec"
			command: "echo"
			args: ["secret-key"]
		}
		PORT: 3000
		DEBUG: true
	}
	
	hooks: {
		onEnter: [
			{
				command: "echo"
				args: ["Entering environment"]
			},
			{
				command: "export"
				source: true
			}
		]
		onExit: {
			command: "echo"  
			args: ["Exiting environment"]
		}
	}
	
	tasks: {
		build: {
			description: "Build the project"
			command: "cargo"
			args: ["build", "--release"]
			env: {
				RUST_LOG: "info"
			}
		}
		test: {
			description: "Run tests"
			command: "cargo"
			args: ["test"]
			dependsOn: ["build"]
		}
	}
}