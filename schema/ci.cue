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
	task!:  string                            // Task name to run
	matrix: [string]: [...string]             // Matrix dimensions (e.g., arch: ["linux-x64", "darwin-arm64"])
	artifacts?: [...#ArtifactDownload]        // Artifacts to download before running
	params?: [string]: string                 // Parameters to pass to the task
})

// Pipeline task reference - either a simple task name or a matrix task
#PipelineTask: string | #MatrixTask

// GitHub Action configuration for stage tasks
#GitHubActionConfig: close({
	uses!: string        // Action reference (e.g., "Mozilla-Actions/sccache-action@v0.2")
	with?: [string]: _   // Action inputs
})

// =============================================================================
// Contributors
// =============================================================================

// Build phases for contributor-injected tasks
#BuildPhase: "bootstrap" | "setup" | "success" | "failure"

// Activation predicate for contributors
// All specified conditions must be true (AND logic)
#ActivationCondition: close({
	// Always active (no conditions)
	always?: bool

	// Runtime type detection (active if project uses any of these runtime types)
	// Values: "nix", "devenv", "container", "dagger", "oci", "tools"
	runtimeType?: [...string]

	// Cuenv source mode detection (for cuenv installation strategy)
	// Values: "git", "nix", "homebrew", "release"
	cuenvSource?: [...string]

	// Secrets provider detection (active if environment uses any of these providers)
	// Values: "onepassword", "aws", "vault", "azure", "gcp"
	secretsProvider?: [...string]

	// Provider configuration detection (active if these config paths are set)
	// Path format: "github.cachix", "github.trustedPublishing.cratesIo"
	providerConfig?: [...string]

	// Task command detection (active if any pipeline task uses these commands)
	// Format: ["gh", "models"] matches tasks with command=["gh", "models", ...]
	taskCommand?: [...string]

	// Task label detection (active if any pipeline task has these labels)
	taskLabels?: [...string]

	// Environment name matching (active only in these environments)
	environment?: [...string]

	// Workspace type detection (active if project has these package managers)
	// Values: "npm", "bun", "pnpm", "yarn", "cargo", "deno"
	workspaceType?: [...string]
})

// Secret reference for phase tasks
#SecretRef: close({
	source!:   string            // CI secret name (e.g., "CACHIX_AUTH_TOKEN")
	cacheKey?: bool | *false     // Include in cache key via salted HMAC
})

// A task contributed to a build phase
#PhaseTask: close({
	id!:       string              // Unique task identifier (e.g., "install-nix")
	phase!:    #BuildPhase         // Target phase (bootstrap, setup, success, failure)
	label?:    string              // Human-readable display name
	command?:  string              // Shell command to execute
	script?:   string              // Multi-line script (alternative to command)
	shell?:    bool | *false       // Wrap command in shell
	env?:      [string]: string    // Environment variables
	secrets?:  [string]: #SecretRef | string  // Secret references (key=env var name)
	dependsOn?: [...string]        // Dependencies on other phase tasks
	priority?: int | *10           // Ordering within phase (lower = earlier)

	// Provider-specific overrides (e.g., GitHub Actions)
	provider?: #PhaseTaskProviderConfig
})

// Provider-specific phase task configuration
#PhaseTaskProviderConfig: close({
	github?: #GitHubActionConfig
})

// Contributor definition
// Contributors inject tasks into build phases based on activation conditions
#Contributor: close({
	id!:    string                    // Contributor identifier (e.g., "nix", "1password")
	when?:  #ActivationCondition      // Activation condition (defaults to always active)
	tasks!: [...#PhaseTask]           // Tasks to contribute when active
})

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

	// Contributors that inject tasks into build phases
	contributors?: [...#Contributor]
})
