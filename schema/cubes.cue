package schema

import "github.com/cuenv/cuenv/schema/cubes"

// ============================================================================
// Cube - Code Generation from CUE Templates
// ============================================================================

// #Cube defines a set of files to generate from CUE configuration.
// Add a cube field to your #Project to enable code generation.
//
// Example:
//   schema.#Project & {
//       name: "my-service"
//       cube: {
//           context: { serviceName: "users" }
//           files: {
//               "package.json": cubes.#JSONFile & {
//                   content: """
//                   { "name": "\(context.serviceName)" }
//                   """
//               }
//           }
//       }
//   }
#Cube: {
	// Files to generate
	// Key is the file path relative to project directory
	files: [string]: cubes.#ProjectFile

	// Optional context data for templating
	// Use this to pass configuration to your cube
	context?: _
}
