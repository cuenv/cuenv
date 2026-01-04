package schema

import "github.com/cuenv/cuenv/schema/codegen"

// ============================================================================
// Codegen - Code Generation from CUE Templates
// ============================================================================

// #Codegen defines a set of files to generate from CUE configuration.
// Add a codegen field to your #Project to enable code generation.
//
// Example:
//   import gen "github.com/cuenv/cuenv/schema/codegen"
//   schema.#Project & {
//       name: "my-service"
//       codegen: {
//           context: { serviceName: "users" }
//           files: {
//               "package.json": gen.#JSONFile & {
//                   content: """
//                   { "name": "\(context.serviceName)" }
//                   """
//               }
//           }
//       }
//   }
#Codegen: {
	// Files to generate
	// Key is the file path relative to project directory
	files: [string]: codegen.#ProjectFile

	// Optional context data for templating
	// Use this to pass configuration to your codegen
	context?: _
}
