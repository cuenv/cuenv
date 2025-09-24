package examples

import "github.com/cuenv/cuenv/schema"

// Example demonstrating environment variable access policies
schema.#Cuenv

// Define reusable policies as CUE values (not part of schema)
_databasePolicy: schema.#Policy & {
	allowTasks: ["migrate", "db_backup", "db_restore"]
	allowExec: ["psql", "pg_dump", "pg_restore"]
}

_apiPolicy: schema.#Policy & {
	allowTasks: ["api_server", "backend_worker", "api_test"]
	allowExec: ["curl", "httpie"]
}

_sensitivePolicy: schema.#Policy & {
	allowTasks: ["deploy", "release"]
	allowExec: ["kubectl", "terraform"]
}

env: {
	// Simple variables without policies (accessible everywhere)
	PORT:         3000
	DEBUG:        true
	APP_NAME:     "PolicyExample"
	DATABASE_URL: "postgres://localhost/mydb"

	// Interpolation still works
	BASE_URL:     "https://api.example.com"
	API_ENDPOINT: "\(BASE_URL)/v1"

	// Database password with policy restrictions
	DB_PASSWORD: {
		value: schema.#Secret
		policies: [_databasePolicy]
	}

	// API key with specific task access
	API_KEY: {
		value: "sk-abc123def456"
		policies: [_apiPolicy]
	}

	// Deployment token with inline policy
	DEPLOY_TOKEN: {
		value: schema.#Secret
		policies: [{
			allowTasks: ["deploy", "ci", "release"]
			allowExec: ["kubectl", "scripts/deploy.sh"]
		}]
	}

	// Admin credentials - combining multiple policies
	ADMIN_SECRET: {
		value: schema.#Secret
		policies: [
			_sensitivePolicy,
			{
				// Additional admin-specific tasks
				allowTasks: ["admin_console", "admin_backup"]
				allowExec: ["scripts/admin.sh"]
			},
		]
	}

	// Development override - no policies means accessible to all
	DEV_OVERRIDE: {
		value: "http://localhost:8080"
		// No policies field - accessible everywhere
	}

	// OnePassword reference with policies
	GITHUB_TOKEN: {
		value: schema.#Secret
		policies: [{
			allowTasks: ["release", "publish"]
			allowExec: ["gh", "git"]
		}]
	}
}

tasks: {
	// Migration task - can access DB_PASSWORD due to _databasePolicy
	migrate: {
		command: "migrate"
		args: ["up", "--database", env.DATABASE_URL]
		// Has access to: all unrestricted vars + DB_PASSWORD
	}

	// API server - can access API_KEY
	api_server: {
		command: "node"
		args: ["server.js", "--port", "\(env.PORT)"]
		// Has access to: all unrestricted vars + API_KEY
	}

	// Test task - no special access
	test: {
		command: "npm"
		args: ["test"]
		// Has access to: only unrestricted vars
		// Cannot access: DB_PASSWORD, API_KEY, DEPLOY_TOKEN, etc.
	}

	// Deployment task - can access deployment secrets
	deploy: {
		command: "deploy"
		args: ["--production", "--token-from-env"]
		// Has access to: unrestricted vars + DEPLOY_TOKEN + ADMIN_SECRET
	}

	// Database backup - inherits access from _databasePolicy
	db_backup: {
		command: "pg_dump"
		args: ["--verbose"]
		// Has access to: unrestricted vars + DB_PASSWORD
	}

	// Admin console - can access admin secrets
	admin_console: {
		command: "admin"
		args: ["--interactive"]
		// Has access to: unrestricted vars + ADMIN_SECRET
	}

	// Build task - basic access only
	build: {
		command: "npm"
		args: ["run", "build"]
		// Has access to: only unrestricted vars
	}

	// Task group example
	database: {
		backup: {
			command: "pg_dump"
			args: [env.DATABASE_URL]
			// Task name would be "database.backup" or similar
			// Would need "database.backup" in allowTasks
		}
		migrate: {
			command: "migrate"
			args: ["up"]
		}
	}

	// CI task - can access deployment tokens
	ci: {
		command: "ci"
		args: ["--deploy"]
		// Has access to: unrestricted vars + DEPLOY_TOKEN
	}

	// Release task - multiple policy access
	release: {
		command: "release"
		args: ["--version", "1.0.0"]
		// Has access to: DEPLOY_TOKEN, ADMIN_SECRET, GITHUB_TOKEN
	}
}

// Example showing exec command access (enforced at runtime)
_execExamples: {
	// cuenv exec -- psql
	// Would have access to DB_PASSWORD (psql is in allowExec)

	// cuenv exec -- kubectl apply
	// Would have access to DEPLOY_TOKEN and ADMIN_SECRET

	// cuenv exec -- bash
	// Would NOT have access to any restricted variables

	// cuenv exec -- curl
	// Would have access to API_KEY only

	// cuenv exec -- gh release create
	// Would have access to GITHUB_TOKEN
}

// Benefits demonstrated:
//
// 1. BACKWARD COMPATIBLE: Simple values like PORT, DEBUG work unchanged
// 2. PROGRESSIVE: Add policies only where needed
// 3. REUSABLE: _databasePolicy, _apiPolicy can be shared
// 4. COMPOSABLE: ADMIN_SECRET combines multiple policies
// 5. SECURE: Tasks only get access to what they need
// 6. CLEAR: Easy to see what each task can access