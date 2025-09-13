package examples

import "github.com/cuenv/cuenv/schema"

schema.#Cuenv

// Environment variables to be loaded after hooks complete
env: {
	NODE_ENV:      "development"
	DATABASE_URL:  "postgresql://localhost:5432/testdb"
	REDIS_URL:     "redis://localhost:6379"
	SERVICE_PORT:  "3000"
	WORKERS:       "4"
}

// Hooks to execute when entering this directory (with delay)
hooks: {
	onEnter: [
		{
			command: "echo"
			args: ["Starting development services..."]
		},
		{
			command: "sleep"
			args: ["2"]  // Simulate docker-compose up or similar
		},
		{
			command: "echo"
			args: ["Services ready on ports 5432, 6379, 3000"]
		},
	]
}

// Task definitions for the environment
tasks: {
	status: {
		description: "Check service status"
		command:     "sh"
		args: ["-c", "echo Database: $DATABASE_URL, Redis: $REDIS_URL, Port: $SERVICE_PORT"]
	}

	verify_all: {
		description: "Verify all environment variables"
		command:     "sh"
		args: ["-c", "env | grep -E 'NODE_ENV|DATABASE_URL|REDIS_URL|SERVICE_PORT|WORKERS'"]
	}
}