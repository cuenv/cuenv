package examples

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
	name: "cube-hello-example"

	cube: {
		context: {
			serviceName: "hello-world"
		}

		files: {
			"package.json": schema.#JSON & {
				mode: "managed"
				content: """
				{
				  "name": "\(context.serviceName)",
				  "version": "1.0.0"
				}
				"""
			}
			"src/main.ts": schema.#TypeScript & {
				mode: "scaffold"
				content: """
				console.log("Hello, \(context.serviceName)!");
				"""
			}
		}
	}
}
