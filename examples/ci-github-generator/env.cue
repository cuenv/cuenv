package examples

import "github.com/cuenv/cuenv/schema"

schema.#Project

let _t = tasks

name: "ci-github-generator"

// The GitHub workflow generator beyond the defaults: triggers, workflow and
// job settings, change detection per lane, and one required gate check.
// Every field is plain data, so `cue export` evaluates this project.
ci: {
	providers: ["github"]
	provider: github: {
		runName: "CI ${{ github.event_name }} by ${{ github.actor }}"
		// The workflow is called `CI` and written to .github/workflows/ci.yml, so
		// another workflow can name it in `workflowRun.workflows`.
		name: "CI"
		file: "ci"
		// A token with no permissions at the workflow level; each job gets the
		// scopes its tasks need.
		permissions:    {}
		jobPermissions: "inferred"
		// Steps never see `GITHUB_TOKEN`, as the pipeline runs pull request code.
		token: false
		checkout: {
			persistCredentials: false
			fetchDepth:         0
		}
		timeoutMinutes: 30
	}
	pipelines: main: {
		mode: "expanded"
		when: {
			pullRequest: {
				types: ["opened", "edited", "synchronize", "reopened"]
				// No `branches`: pull requests into any branch.
			}
			mergeGroup: true
			branch:     "main"
		}
		concurrency: {
			group: "${{ github.workflow }}-${{ github.head_ref || github.ref }}"
			cancelInProgress: {pullRequest: true}
		}
		tasks: [_t.plan, _t.lint, _t.test, _t.shard, _t.publish]

		// Lanes name tasks of this pipeline; a lane runs when one of them is
		// affected by the change.
		changes: lanes: {
			rust: tasks: ["lint", "test"]
			web: tasks: ["shard"]
		}
		// One stable check for a branch ruleset: it fails when any job failed
		// or was cancelled, and passes skipped lanes. `publish` must succeed.
		gate: {
			name:     "ci-ok"
			required: ["publish"]
		}
	}
}

tasks: {
	plan: schema.#Task & {
		command:  "echo"
		args: ["matrix=[]"]
		hermetic: false
		// Written to $GITHUB_OUTPUT by the task, exposed as a job output.
		ci: github: outputs: ["matrix"]
	}

	lint: schema.#Task & {
		command:  "echo"
		args: ["linting"]
		hermetic: false
		ci: github: {
			timeoutMinutes: 10
			// The job ID stays `lint`; the Actions UI shows this name.
			jobName: "Lint the Rust crates"
			container: {
				image: "rust:1.100@sha256:0000000000000000000000000000000000000000000000000000000000000000"
				env: CI: "1"
			}
			defaults: run: workingDirectory: "."
		}
	}

	test: schema.#Task & {
		command:  "echo"
		args: ["testing"]
		hermetic: false
		ci: github: {
			// Run an extra step around the task's own.
			steps: before: [{
				name: "Show the runner"
				run:  "uname -a"
			}]
		}
	}

	// Fans out over a matrix that the `plan` job computed.
	shard: schema.#Task & {
		command:   "echo"
		args: ["sharding"]
		hermetic:  false
		dependsOn: [_t.plan]
		ci: github: matrix: {
			include:  (schema.#GitHubJobOutput & {job: "plan", output: "matrix"}).ref
			failFast: false
			runsOn:   "${{ matrix.runner }}"
		}
	}

	publish: schema.#Task & {
		command:   "echo"
		args: ["publishing"]
		hermetic:  false
		dependsOn: [_t.lint, _t.test]
		ci: github: {
			// Run after the other jobs even when a lane was skipped.
			if: "!cancelled() && !contains(needs.*.result, 'failure')"
			environment: {
				name: "production"
				url:  "https://example.com"
			}
		}
	}
}
