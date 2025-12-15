package _examples

import (
	"github.com/cuenv/cuenv/schema"
	"github.com/cuenv/cuenv/schema/cubes"
)

schema.#Project & {
	name: "cube-hello-example"

	cube: {
		context: {
			serviceName: "hello-world"
		}

		files: {
			"package.json": cubes.#JSONFile & {
				mode: "managed"
				content: """
				{
				  "name": "\(context.serviceName)",
				  "version": "1.0.0"
				}
				"""
			}
			"src/main.ts": cubes.#TypeScriptFile & {
				mode: "scaffold"
				content: """
				console.log("Hello, \(context.serviceName)!");
				"""
			}
		}
	}
}
