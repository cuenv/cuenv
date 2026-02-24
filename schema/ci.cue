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
	// Trusted publishing via OIDC (no secrets needed)
	trustedPublishing?: close({
		cratesIo?: bool // Enable trusted publishing for crates.io
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
	type:   "matrix"                          // Discriminator for CUE disjunction (distinguishes from #TaskNode)
	task!:  #TaskNode                         // Task reference (CUE ref for compile-time validation)
	matrix: [string]: [...string]             // Matrix dimensions (e.g., arch: ["linux-x64", "darwin-arm64"])
	artifacts?: [...#ArtifactDownload]        // Artifacts to download before running
	params?: [string]: string                 // Parameters to pass to the task
})

// Pipeline task reference - either a direct task reference or a matrix task
#PipelineTask: #TaskNode | #MatrixTask

// GitHub Action configuration for contributor tasks
#GitHubActionConfig: close({
	uses!: string        // Action reference (e.g., "Mozilla-Actions/sccache-action@v0.2")
	with?: [string]: _   // Action inputs
})

// =============================================================================
// Contributors
// =============================================================================

// Auto-association rules for contributors
// Defines how user tasks are automatically connected to contributor tasks
#AutoAssociate: close({
	// Commands that trigger auto-association (e.g., ["bun", "bunx"])
	command?: [...string]
	// Task to inject as dependency (e.g., "cuenv:contributor:bun.workspace.setup")
	injectDependency?: string
})

// Secret reference for contributor tasks
#SecretRef: close({
	source!:   string            // CI secret name (e.g., "CACHIX_AUTH_TOKEN")
	cacheKey?: bool | *false     // Include in cache key via salted HMAC
})

// Execution condition for contributor tasks
#TaskCondition: "on_success" | "on_failure" | "always"

// Provider-specific task configuration
#TaskProviderConfig: close({
	github?: #GitHubActionConfig
})

// Contributor task definition
// Tasks injected into the DAG by contributors
#ContributorTask: close({
	// Task identifier (will be prefixed with cuenv:contributor:)
	id!: string
	// Shell command to execute
	command?: string
	// Command arguments
	args?: [...string]
	// Multi-line script (alternative to command)
	script?: string
	// Wrap command in shell
	shell?: bool | *false
	// Environment variables
	env?: [string]: string
	// Secret references (key=env var name)
	secrets?: [string]: #SecretRef | string
	// Input files/patterns for caching
	inputs?: [...string]
	// Output files/patterns for caching
	outputs?: [...string]
	// Whether task requires hermetic execution
	hermetic?: bool | *false
	// Dependencies on other tasks
	dependsOn?: [...string]
	// Human-readable description
	description?: string
	// Human-readable display name
	label?: string
	// Ordering priority (lower = earlier)
	priority?: int | *10
	// Execution condition (on_success, on_failure, always)
	condition?: #TaskCondition
	// Provider-specific overrides (e.g., GitHub Actions)
	provider?: #TaskProviderConfig
})

// Contributor activation condition
// All specified conditions must be true (AND logic)
#ActivationCondition: close({
	// Always active (no conditions)
	always?: bool

	// Workspace membership detection (active if project is member of these workspace types)
	// Values: "npm", "bun", "pnpm", "yarn", "cargo", "deno"
	workspaceMember?: [...string]

	// Runtime type detection (active if project uses any of these runtime types)
	// Values: "nix", "devenv", "container", "dagger", "oci", "tools"
	runtimeType?: [...string]

	// Cuenv source mode detection (for cuenv installation strategy)
	// Values: "git", "nix", "homebrew", "release"
	cuenvSource?: [...string]

	// Secrets provider detection (active if environment uses any of these providers)
	// Values: "onepassword", "infisical", "aws", "vault", "azure", "gcp"
	secretsProvider?: [...string]

	// Provider configuration detection (active if these config paths are set)
	// Path format: "github.cachix", "github.trustedPublishing.cratesIo"
	providerConfig?: [...string]

	// Task command detection (active if any task uses these commands)
	taskCommand?: [...string]

	// Task label detection (active if any task has these labels)
	taskLabels?: [...string]

	// Environment name matching (active only in these environments)
	environment?: [...string]
})

// Contributor definition
// Contributors inject tasks into the DAG based on activation conditions
#Contributor: close({
	// Contributor identifier (e.g., "bun.workspace", "nix", "1password")
	id!: string
	// Activation condition (defaults to always active)
	when?: #ActivationCondition
	// Tasks to contribute when active
	tasks!: [...#ContributorTask]
	// Auto-association rules for user tasks
	autoAssociate?: #AutoAssociate
})

// Pipeline generation mode
// - "thin": Generate minimal workflow with cuenv ci orchestration (default)
//   Structure: bootstrap contributors → cuenv ci --pipeline <name> → finalizer contributors
// - "expanded": Generate full workflow with all tasks as individual jobs/steps
//   Structure: All tasks expanded inline with proper dependencies
#PipelineMode: "thin" | "expanded"

// CI provider names for workflow generation
// Used to specify which CI providers should emit workflow manifests
#CIProvider: "github" | "buildkite" | "gitlab"

#Pipeline: close({
	// Generation mode for this pipeline (default: "thin")
	mode?: #PipelineMode | *"thin"

	// CI providers to emit workflows for (overrides global ci.providers for this pipeline)
	// If specified, completely replaces the global providers list for this pipeline
	providers?: [...#CIProvider]

	environment?: string // environment for secret resolution (e.g., "production")
	when?:        #PipelineCondition

	// Tasks to run - can be simple task names or matrix task objects
	tasks?: [...#PipelineTask]

	derivePaths?: bool // whether to derive trigger paths from task inputs
	provider?:    #ProviderConfig
})

#CI: close({
	// CI providers to emit workflows for (e.g., ["github", "buildkite"])
	// If not specified, no workflows are emitted (explicit configuration required)
	// Per-pipeline providers can override this global setting
	providers?: [...#CIProvider]

	pipelines?: [string]: #Pipeline
	provider?:  #ProviderConfig

	// Contributors that inject tasks into the DAG
	contributors?: [...#Contributor]
})
