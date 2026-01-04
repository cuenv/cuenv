package examples

import (
	"github.com/cuenv/cuenv/schema"
	gen "github.com/cuenv/cuenv/schema/codegen"
)

schema.#Project & {
	name: "codegen-hello-example"

	codegen: {
		context: {
			serviceName: "hello-world"
		}

		files: {
			"package.json": gen.#JSONFile & {
				mode: "managed"
				content: """
				{
				  "name": "\(context.serviceName)",
				  "version": "1.0.0"
				}
				"""
			}
			"src/main.ts": gen.#TypeScriptFile & {
				mode: "scaffold"
				content: """
				console.log("Hello, \(context.serviceName)!");
				"""
			}
		}
	}
}
