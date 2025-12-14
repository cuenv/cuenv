// Simple Node.js API Blueprint Example
package blueprint

import (
	"encoding/json"
	"github.com/cuenv/cuenv-codegen/schemas/code"
)

// Project context
#Context: {
	project: {
		name:        string & =~"^[a-z][a-z0-9-]*$"
		version:     string
		description: string
		port:        int | *3000
	}
	features: {
		database: bool
		testing:  bool
	}
}

context: #Context & {
	project: {
		name:        "my-api"
		version:     "0.1.0"
		description: "A simple REST API service"
		port:        8080
	}
	features: {
		database: true
		testing:  true
	}
}

// Files to generate
files: {
	"package.json": code.#JSON & {
		mode: "managed"
		content: json.Marshal({
			name:        context.project.name
			version:     context.project.version
			description: context.project.description
			type:        "module"
			scripts: {
				dev:   "tsx src/main.ts"
				build: "tsc"
				start: "node dist/main.js"
				if context.features.testing {
					test: "vitest"
				}
			}
			dependencies: {
				express: "^4.18.0"
				dotenv:  "^16.0.0"
				if context.features.database {
					pg: "^8.11.0"
				}
			}
			devDependencies: {
				typescript:   "^5.3.0"
				"@types/node": "^20.10.0"
				tsx:          "^4.7.0"
				if context.features.testing {
					vitest: "^1.0.0"
				}
			}
		})
	}

	"src/main.ts": code.#TypeScript & {
		mode: "scaffold"
		format: {
			indent:     "space"
			indentSize: 2
			quotes:     "single"
		}
		content: """
			import express from 'express';
			\(context.features.database ? "import { db } from './services/database';" : "")

			const app = express();
			const PORT = process.env.PORT || \(context.project.port);

			app.use(express.json());

			app.get('/health', (req, res) => {
			  res.json({ status: 'ok', service: '\(context.project.name)' });
			});

			\(context.features.database ? """
			app.get('/users', async (req, res) => {
			  const users = await db.query('SELECT * FROM users');
			  res.json(users);
			});
			""" : "")

			app.listen(PORT, () => {
			  console.log('\(context.project.name) running on port ' + PORT);
			});
			"""
	}

	"tsconfig.json": code.#JSON & {
		mode: "managed"
		content: json.Marshal({
			compilerOptions: {
				target:           "ES2022"
				module:           "NodeNext"
				moduleResolution: "NodeNext"
				outDir:           "./dist"
				strict:           true
				esModuleInterop:  true
				skipLibCheck:     true
			}
			include: ["src/**/*"]
			exclude: ["node_modules", "dist"]
		})
	}

	".gitignore": code.#Code & {
		language: "text"
		mode:     "managed"
		content: """
			node_modules
			dist
			.env
			*.log
			"""
	}

	if context.features.database {
		"src/services/database.ts": code.#TypeScript & {
			mode: "scaffold"
			content: """
				import { Pool } from 'pg';

				let pool: Pool;

				export async function initDatabase() {
				  pool = new Pool({
				    connectionString: process.env.DATABASE_URL,
				  });

				  await pool.query('SELECT 1'); // Test connection
				  console.log('✓ Database connected');
				}

				export function getDb() {
				  return pool;
				}

				export const db = {
				  query: async (sql: string, params?: any[]) => {
				    const result = await pool.query(sql, params);
				    return result.rows;
				  }
				};
				"""
		}
	}
}
