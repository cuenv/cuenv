package examples

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "task-captures"

tasks: {
	// A build task that emits structured output.
	// Two named captures pull specific values from stdout.
	build: schema.#Task & {
		command: "echo"
		args: ["Build complete. version=1.2.3-abc1234 size=4.2MB"]
		captures: {
			version: {
				pattern: "version=([^ ]+)"
				source:  "stdout"
			}
			size: {
				pattern: "size=([^ ]+)"
				source:  "stdout"
			}
		}
	}

	// A task whose output references a sibling capture is not currently
	// supported — captures are resolved after execution and are surfaced
	// through CI annotations, not as inter-task output refs.
	// Use #TaskOutput (tasks.build.stdout) for whole-stream cross-task deps.
	report: schema.#Task & {
		command: "echo"
		args: ["reporting"]
		dependsOn: [build]
	}
}

ci: {
	providers: ["github"]

	pipelines: {
		default: {
			tasks: [tasks.build, tasks.report]

			// Captures from tasks can be surfaced as CI step annotations.
			// The resolved values appear in the GitHub job summary table.
			annotations: {
				"Build version": schema.#TaskCaptureRef & {
					cuenvTask:    "build"
					cuenvCapture: "version"
				}
				"Bundle size": schema.#TaskCaptureRef & {
					cuenvTask:    "build"
					cuenvCapture: "size"
				}
			}
		}
	}
}
