package examples

import shared "github.com/cuenv/cuenv/examples/imported-task-dirs/shared"

name: "imported-task-dirs"

tasks: {
	// Defaults to the imported task definition directory:
	// examples/imported-task-dirs/shared
	definition: shared.tasks.pwd

	// Runs from the directory of this importing env.cue:
	// examples/imported-task-dirs
	caller: shared.tasks.pwd & {
		dir: from: "caller"
	}

	// Runs from a path relative to the imported task definition:
	// examples/imported-task-dirs/shared/fixtures
	definitionSubdir: shared.tasks.pwd & {
		dir: {
			from: "definition"
			path: "fixtures"
		}
	}

	// Runs from a path relative to the importing env.cue:
	// examples/imported-task-dirs/fixtures
	callerSubdir: shared.tasks.pwd & {
		dir: {
			from: "caller"
			path: "fixtures"
		}
	}

	// Legacy form, still relative to the CUE module root:
	// examples/imported-task-dirs
	moduleRelative: shared.tasks.pwd & {
		dir: "examples/imported-task-dirs"
	}
}
