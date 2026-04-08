package schema

// =============================================================================
// Service — long-running supervised process
// =============================================================================
//
// Services live alongside tasks on a project but execute under different
// rules: they must reach a readiness state, are kept alive across the
// session, restart according to policy, and tear down on `cuenv down`.

#Service: {
	_cuenvPrefix: string | *""
	_cuenvSelf:   string | *""
	_name: string | *(_cuenvPrefix + _cuenvSelf)

	// Type discriminator — required so #Service never matches #Task
	type: "service"

	// Command-based execution (mutually exclusive with script)
	command?: string
	args?: [...(string | #TaskOutputRef)]

	// Script-based execution
	script?:       string
	scriptShell?:  #ScriptShell | *"bash"
	shellOptions?: #ShellOptions

	// Environment variables (same shape as #Task)
	env?: [string]: #EnvironmentVariable | #TaskOutputRef

	// Working directory override
	dir?: string

	// Dependencies — may reference tasks OR services. Tasks must complete;
	// services must become ready before this service starts.
	dependsOn?: [...(#TaskNode | #Service)]

	// Labels for discovery via #ServiceMatcher (mirrors #TaskMatcher)
	labels?: [...string]

	// Human-readable description
	description?: string

	// Runtime override for this service
	runtime?: #Runtime

	// Readiness probe (single probe per service in v1)
	readiness?: #Readiness

	// Restart policy
	restart?: #RestartPolicy

	// File watcher → restart loop
	watch?: #Watch

	// Log handling
	logs?: #ServiceLogs

	// Shutdown behavior
	shutdown?: #Shutdown

	// Hard kill if startup → ready exceeds this
	timeout?: string
}

// =============================================================================
// Readiness Probes
// =============================================================================

#Readiness: #ReadinessPort
	| #ReadinessHttp
	| #ReadinessLog
	| #ReadinessCommand
	| #ReadinessDelay

#ReadinessCommon: {
	// Time between probe attempts
	interval?: string | *"500ms"
	// Max time to reach ready before considered failed
	timeout?: string | *"60s"
	// Initial delay before first probe attempt
	initialDelay?: string | *"0s"
}

#ReadinessPort: close({
	#ReadinessCommon
	kind: "port"
	// TCP port on localhost (or `host` if specified)
	port!: int & >0 & <65536
	host?: string | *"127.0.0.1"
})

#ReadinessHttp: close({
	#ReadinessCommon
	kind: "http"
	url!: string
	// Expected status code(s); default 2xx
	expectStatus?: [...int] | *[200, 201, 202, 203, 204, 205, 206]
	method?: "GET" | "HEAD" | *"GET"
})

#ReadinessLog: close({
	#ReadinessCommon
	kind: "log"
	// Regex; first match on stdout or stderr declares ready
	pattern!: string
	source?:  "stdout" | "stderr" | "either" | *"either"
})

#ReadinessCommand: close({
	#ReadinessCommon
	kind: "command"
	// Exit 0 = ready. Runs in service env, not service process.
	command!: string
	args?: [...string]
})

#ReadinessDelay: close({
	kind: "delay"
	// Dumb sleep; escape hatch only
	delay!: string
})

// =============================================================================
// Restart Policy
// =============================================================================

#RestartPolicy: close({
	// never:         crash → mark failed, abort dependents
	// onFailure:     restart on non-zero exit (default for services)
	// always:        restart on any exit
	// unlessStopped: like always, but `cuenv down svc` is sticky
	mode?: "never" | "onFailure" | "always" | "unlessStopped" | *"onFailure"

	// Exponential backoff between restarts
	backoff?: close({
		initial?: string | *"1s"
		max?:     string | *"30s"
		factor?:  number | *2.0
	})

	// Cap restarts within a sliding window. Exceeding marks the service
	// failed and aborts dependents.
	maxRestarts?: int | *5
	window?:      string | *"60s"
})

// =============================================================================
// File Watcher
// =============================================================================

#Watch: close({
	// Glob patterns relative to project root
	paths!: [...string]
	// Patterns to ignore (gitignore syntax)
	ignore?: [...string]
	// Debounce window for batched changes
	debounce?: string | *"200ms"
	// What to do on change. v1 supports "restart" only.
	on?: "restart" | *"restart"
	// Optional dependency tasks to re-run before restart
	// (e.g., rebuild a binary). Treated as ad-hoc additions to the DAG.
	rebuild?: [...#TaskNode]
})

// =============================================================================
// Logs
// =============================================================================

#ServiceLogs: close({
	// Stream prefix shown in multiplexed output. Defaults to service name.
	prefix?: string
	// ANSI color hint for renderers (renderer chooses if absent)
	color?: "red" | "green" | "yellow" | "blue" | "magenta" | "cyan" | "white"
	// Persist to file under .cuenv/run/<project>/logs/<svc>.log
	persist?: bool | *true
})

// =============================================================================
// Shutdown
// =============================================================================

#Shutdown: close({
	// SIGTERM by default; SIGINT/SIGKILL for stubborn programs
	signal?: "SIGTERM" | "SIGINT" | "SIGHUP" | "SIGQUIT" | *"SIGTERM"
	// Grace period before SIGKILL
	timeout?: string | *"10s"
})

// =============================================================================
// Service Matcher (mirrors #TaskMatcher for discovery)
// =============================================================================

#ServiceMatcher: close({
	labels?: [...string]
	parallel: bool | *true
})
