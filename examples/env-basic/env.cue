package examples

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "env-basic"

env: {
	// Basic Types
	DATABASE_URL: "postgres://localhost/mydb"
	DEBUG:        true
	PORT:         3000

	// Interpolation
	BASE_URL:     "https://api.example.com"
	API_ENDPOINT: "\(BASE_URL)/v1"
}

