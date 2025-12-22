package schema

// Workflow dispatch input definition for manual triggers
#WorkflowDispatchInput: close({
	description!: string
	required?:    bool
	default?:     string
	type?:        "string" | "boolean" | "choice" | "environment"
	options?: [...string] // only valid when type is "choice"
})

// Workflow dispatch inputs map
#WorkflowDispatchInputs: [string]: #WorkflowDispatchInput

#PipelineCondition: close({
	pullRequest?:   bool
	branch?:        string | [...string]
	tag?:           string | [...string]
	defaultBranch?: bool
	scheduled?:     string | [...string]           // cron expression(s)
	manual?:        bool | #WorkflowDispatchInputs // simple bool OR with inputs
	release?: [...string]                          // release event types e.g. ["published"]
})

// GitHub Actions provider configuration
#GitHubConfig: close({
	runner?: string | [...string]
	// Runner mapping for matrix dimensions (e.g., arch -> runner name)
	runners?: close({
		arch?: [string]: string
	})
	cachix?: close({
		name!:       string
		authToken?:  string
		pushFilter?: string
	})
	artifacts?: close({
		paths?:          [...string]
		ifNoFilesFound?: "warn" | "error" | "ignore"
	})
	pathsIgnore?: [...string]
	permissions?: [string]: string
})

// Buildkite provider configuration
#BuildkiteConfig: close({
	queue?:     string
	useEmojis?: bool
	plugins?: [...close({
		name!:   string
		config?: _
	})]
})

// GitLab CI provider configuration
#GitLabConfig: close({
	image?: string
	tags?: [...string]
	cache?: close({
		key?:   string
		paths?: [...string]
	})
})

// Provider-specific configuration
#ProviderConfig: close({
	github?:    #GitHubConfig
	buildkite?: #BuildkiteConfig
	gitlab?:    #GitLabConfig
})

// Artifact download configuration for pipeline tasks
#ArtifactDownload: close({
	from!:   string       // Source task name (must have outputs)
	to!:     string       // Base directory to download artifacts into
	filter?: string | *"" // Glob pattern to filter matrix variants (e.g., "*stable")
})

// Matrix task configuration for pipeline
#MatrixTask: close({
	task!:  string                            // Task name to run
	matrix: [string]: [...string]             // Matrix dimensions (e.g., arch: ["linux-x64", "darwin-arm64"])
	artifacts?: [...#ArtifactDownload]        // Artifacts to download before running
	params?: [string]: string                 // Parameters to pass to the task
})

// Pipeline task reference - either a simple task name or a matrix task
#PipelineTask: string | #MatrixTask

#Pipeline: close({
	name:         string
	environment?: string // environment for secret resolution (e.g., "production")
	when?:        #PipelineCondition

	// Tasks to run - can be simple task names or matrix task objects
	tasks?: [...#PipelineTask]

	derivePaths?: bool // whether to derive trigger paths from task inputs
	provider?:    #ProviderConfig
})

#CI: close({
	pipelines: [...#Pipeline]
	provider?: #ProviderConfig
})
