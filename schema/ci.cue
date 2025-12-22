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

#Pipeline: close({
	name:         string
	environment?: string // environment for secret resolution (e.g., "production")
	when?:        #PipelineCondition

	// Either specify tasks manually OR use release: true to auto-generate from release config
	tasks?: [...string]

	// When true, auto-generates build matrix and publish jobs from release.targets and release.backends
	// This replaces manual task specification for release workflows
	release?: bool

	derivePaths?: bool // whether to derive trigger paths from task inputs
	provider?:    #ProviderConfig
})

#CI: close({
	pipelines: [...#Pipeline]
	provider?: #ProviderConfig
})
