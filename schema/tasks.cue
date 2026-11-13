package schema

import "strings"

// =============================================================================
// Task API v2 - Explicit Types
// =============================================================================
//
// Users annotate tasks with their type to unlock specific semantics:
//   - #Task: Single command or script
//   - #TaskGroup: Parallel execution (all children run concurrently)
//   - #TaskSequence: Sequential execution (steps run in order)
//
// Example:
//   tasks: {
//       build: #Task & { command: "cargo build" }
//       check: #TaskGroup & {
//           type: "group"
//           lint: #Task & { command: "cargo clippy" }
//           test: #Task & { command: "cargo test" }
//       }
//       deploy: #TaskSequence & [
//           #Task & { command: "build" },
//           #Task & { command: "push" },
//       ]
//   }

// Union of all task types - explicit typing required
#TaskNode: #Task | #TaskGroup | #TaskSequence

// =============================================================================
// Task Output References
// =============================================================================
//
// Tasks produce stdout, stderr, and exitCode fields that evaluate to
// #TaskOutputRef structs. These can be referenced by other tasks:
//
//   tasks: {
//       tmpdir: { command: "mktemp", args: ["-d"] }
//       work: {
//           command: "ls"
//           env: TEMP_DIR: tasks.tmpdir.stdout
//       }
//   }
//
// Field names use no underscore prefix so they appear in JSON output.
// The Rust executor resolves these at runtime after upstream tasks complete.

#TaskOutputRef: {
	cuenvOutputRef: true
	cuenvProject:   string
	cuenvTask:      string
	cuenvOutput:    "stdout" | "stderr" | "exitCode"
}

// =============================================================================
// Script Shell Configuration
// =============================================================================

// Interpreter for script-based tasks
#ScriptShell: "bash" | "sh" | "zsh" | "fish" | "nu" | "powershell" | "pwsh" | "python" | "node" | "ruby" | "perl"

// Shell options (for bash-like shells)
#ShellOptions: {
	errexit?:  bool | *true  // -e: exit on error
	nounset?:  bool | *true  // -u: error on undefined vars
	pipefail?: bool | *true  // -o pipefail: fail on pipe errors (bash/zsh only)
	xtrace?:   bool | *false // -x: debug/trace mode
}

// =============================================================================
// Task Cache Policy
// =============================================================================

#TaskCacheMode: "never" | "read" | "write" | "read-write"

#TaskCachePolicy: {
	// Cache mode (default: never)
	mode?: #TaskCacheMode | *"never"
	// Maximum age for cache reads, e.g. "30m", "1h", "infinite"
	maxAge?: string
}

// =============================================================================
// Hermeticity
// =============================================================================

// Object form of a task's `hermetic` field. Setting it always means
// hermeticity is on; there would be nothing to configure otherwise.
#HermeticPassthrough: string & !~"(?i)^HOME$"

#Hermetic: {
	// Host environment variable names the action is allowed to depend on.
	//
	// Their host values are folded into the cache key. HOME is deliberately
	// excluded: use hermetic: false when a host task must use the caller's home.
	// Hermetic tasks default to HOME=/home/builder, USER=builder, and
	// LOGNAME=builder. The host executor uses a Cuenv-managed writable HOME
	// if the fixed path is unavailable, and keys the action on the selected path.
	// Host values of names not listed here never enter the key: cuenv will not
	// silently key a result on the machine it was computed on. Omit the field
	// when a task's result does not depend on the host environment, which is
	// the portable case and the one a shared cache can serve.
	passthrough?: [...#HermeticPassthrough]

	// Filesystem isolation tier. Defaults to "dir".
	//
	//   "dir"  — the task runs in a per-action directory populated from its
	//            declared `inputs`, and only declared `outputs` are projected
	//            back. This isolates relative workspace reads and writes, but
	//            is not an OS security boundary: absolute host paths remain
	//            reachable. A symlinked input is followed and staged as a
	//            regular file; output symlinks are rejected.
	//   "none" — the task runs in the project directory and can read and
	//            write anything on the machine. Incomplete `inputs` produce
	//            stale cache hits, so entries recorded here are only ever as
	//            trustworthy as the declaration.
	//
	// Directory isolation is the default. Reach for "none" when a task must
	// touch the live checkout.
	sandbox?: #Sandbox
}

// Workspace filesystem isolation tiers. Strict OS-level confinement is not
// listed until it is implemented.
#Sandbox: "dir" | "none"

// Working directory base for object-shaped task dir values.
#TaskDirBase: "definition" | "caller" | "module"

// Working directory override. Resolves relative to the executable task
// definition, the importing/re-exporting caller, or the module.
#TaskDir: {
	from: #TaskDirBase | *"definition"
	path: string | *"."
}

// =============================================================================
// Single Executable Task
// =============================================================================

// Hidden helper fields used to derive fully-qualified task names from label aliases.
// Named tasks and group children get these from schema templates; sequence items
// still rely on the Go bridge because CUE does not yet support aliases on list
// elements.
#Task: {
	// GitHub token requirements for the job executing this task.
	ci?:           #TaskCI
	_cuenvPrefix:  string | *""
	_cuenvSelf:    string | *""
	_cuenvProject: string | *""
	// What this task is and which project it belongs to. Reserved.
	cuenvRef: {kind: "task", project: _cuenvProject, name: _name}

	// Executable tasks use command/script; task groups use type: "group".
	type?: _|_

	// Fully-qualified task name used by runtime output references.
	// For named tasks and group children this is derived in schema via label aliases.
	// The Go bridge fills sequence item names (pipeline[0], etc.) before JSON serialization.
	// The default empty string preserves evaluation when no name has been injected yet.
	_name: string | *(_cuenvPrefix + _cuenvSelf)

	// Runtime output references - resolve to #TaskOutputRef structs.
	// Other tasks can reference these (e.g., tasks.tmpdir.stdout) to pass
	// runtime outputs between tasks. Resolved by Rust executor, not CUE.
	stdout: #TaskOutputRef & {cuenvProject: _cuenvProject, cuenvTask: _name, cuenvOutput: "stdout"}
	stderr: #TaskOutputRef & {cuenvProject: _cuenvProject, cuenvTask: _name, cuenvOutput: "stderr"}
	exitCode: #TaskOutputRef & {cuenvProject: _cuenvProject, cuenvTask: _name, cuenvOutput: "exitCode"}

	// Every file this task declares in `outputs`, as an input of another
	// task: `inputs: [content.tasks.build.outputFiles]`. Reading them also
	// makes the reader depend on this task. Reserved.
	outputFiles: #TaskOutputFilesRef & {cuenvProject: _cuenvProject, cuenvTask: _name}

	// Command-based execution
	command?: string
	args?: [...(string | #TaskOutputRef)]

	// Script-based execution (mutually exclusive with command)
	script?:       string
	scriptShell?:  #ScriptShell | *"bash"
	shellOptions?: #ShellOptions

	// An executable task has exactly one execution mode.
	({command!: string, script?: _|_} | {script!: string, command?: _|_})

	// Environment variables
	// Host HOME is never passed through by a task env marker. A non-hermetic
	// host task inherits the caller's environment without declaring a marker.
	env?: {
		[string]: #EnvironmentVariable | #TaskOutputRef
		HOME?:    (string & !~"(?i)^cuenv:passthrough:HOME$") | int | bool | #Secret | #InterpolatedEnv | #EnvironmentVariableWithPolicies | #TaskOutputRef
	}

	// Working directory override. Defaults to the task definition directory.
	dir: #TaskDir

	// Hermeticity settings.
	//
	// When true (default), the task opts into hermetic execution: it is
	// eligible for the action cache, and its cache key records only what it
	// declares. When false, the task runs directly in the workspace with the
	// ambient host environment and is never cached, because the key would
	// describe a fraction of what produced the result.
	//
	// The object form additionally declares which host environment variables
	// the action may depend on. See #Hermetic.
	hermetic?: bool | #Hermetic | *true

	// Dependencies - reference other tasks or images for compile-time validation
	dependsOn?: [...(#TaskNode | #ContainerImage)]

	// Labels for task discovery via #TaskMatcher
	labels?: [...string]

	// Input files/patterns for caching and hermetic execution
	inputs?: [...#Input]

	// A string input is a path of this project and cannot leave it. The
	// error is keyed on the input as written and unified with it, so the
	// user's own line is reported.
	_cuenvInputCheck: {
		if inputs != _|_ for v in inputs if (v & string) != _|_ if strings.Contains("/"+(v & string)+"/", "/../") {
			(v & string): v & error(_cuenvLeavesProject)
		}
	}

	// Output files/patterns for caching
	outputs?: [...string]

	// Per-task task-result cache policy
	cache?: #TaskCachePolicy

	// Human-readable description
	description?: string

	// Runtime override for this task
	runtime?: #Runtime

	// Task parameter definitions for CLI arguments
	params?: #TaskParams

	// Execution policies
	timeout?: string // e.g. "30m"
	retry?: {
		attempts: int | *3
		delay?:   string // e.g. "5s"
	}
	continueOnError?: bool | *false

	// Named regex captures extracted from task stdout/stderr after execution
	captures?: [string]: #TaskCapture
}

// =============================================================================
// Parallel Execution (Task Group)
// =============================================================================

#TaskGroup: {
	_cuenvPrefix:  string | *""
	_cuenvSelf:    string | *""
	_cuenvProject: string | *""
	let _groupProject = _cuenvProject
	// What this group is and which project it belongs to. Reserved: a group
	// cannot have a child named `cuenvRef`.
	cuenvRef: {kind: "group", project: _cuenvProject, name: _name}

	// Fully-qualified group path used to derive child task names.
	_name: string | *(_cuenvPrefix + _cuenvSelf)

	// Type discriminator - must be "group"
	type: "group"

	// Dependencies on other tasks
	dependsOn?: [...#TaskNode]

	// Limit concurrent executions (0 = unlimited)
	maxConcurrency?: int

	// Human-readable description
	description?: string

	// Named children - all run concurrently (as direct fields).
	// Task and group children derive their fully-qualified names from the
	// current group's _name. Sequence children keep their name context in the
	// bridge, which can see list indexes.
	// Select child validation from the group's existing type discriminator.
	// This avoids applying the recursive group pattern to every task-shaped
	// object while preserving the public task fields and flat child syntax.
	// A sequence child is checked as a #TaskSequence and only stamped with the
	// project here. Spelling its members out as a fresh `#Task | #TaskGroup`
	// disjunction made every task with a struct input (a file reference)
	// cost minutes under CUE v0.17 wherever it met #TaskNode: a pipeline
	// entry, a dependsOn entry, a matrix entry's task.
	let _childrenByType = {
		group: {
			// `cuenvRef` is reserved for what cuenv writes on a group.
			{[childName= !~"^(type|dependsOn|maxConcurrency|description|cuenvRef)$"]: ((#Task | #TaskGroup) & {
				_cuenvPrefix:  _name + "."
				_cuenvSelf:    childName
				_cuenvProject: _groupProject
			}) | (#TaskSequence & [...({_cuenvProject: _groupProject, ...} | [...])])
			}
		}
	}
	_childrenByType[type]
}

// =============================================================================
// Sequential Execution (Task Sequence)
// =============================================================================

// A sequence is simply an ordered list of task nodes - run in order
#TaskSequence: [...#TaskNode]

// =============================================================================
// Task Parameters
// =============================================================================

#TaskParams: {
	// Positional arguments (order matters, consumed left-to-right)
	positional?: [...#Param]
	// Named arguments are declared as direct fields (--flag style)
	[!~"^positional$"]: #Param
}

#Param: {
	description?: string
	required?:    bool | *false
	default?:     string
	type?:        "string" | "bool" | "int" | *"string"
	short?:       =~"^[a-zA-Z]$"
}

// =============================================================================
// Task Inputs
// =============================================================================

// Accepted task inputs:
// - string: File path, directory, or glob pattern of this project
// - #TaskOutput: The declared outputs of a task of this project, by name
// - #TaskOutputFilesRef: The declared outputs of a task, by reference:
//   `tasks.<name>.outputFiles`, or `<pkg>.tasks.<name>.outputFiles` through
//   the import of another project
// - #ProjectFiles: Files of another project, by import (`<pkg>.#Files`)
// - #ModuleFiles: Files anywhere in the module, from the module root
#Input: string | #TaskOutput | #TaskOutputFilesRef | #ProjectFiles | #ModuleFiles

// What a string input with a `..` segment is told: it would leave the
// project's directory, where no reader of inputs can follow it.
_cuenvLeavesProject: "an input cannot leave the project directory: import the project that holds these files and use `<pkg>.#Files & {paths: [...]}` (or `<pkg>.tasks.<name>.outputFiles` for files a task builds); for files outside every project use `schema.#ModuleFiles & {paths: [...]}` with paths from the module root (see https://cuenv.dev/how-to/run-tasks/#files-of-another-project)"

// One path of a file reference: relative to its anchor and inside it, and
// never into the repository's metadata. Every reader of inputs sees the same
// string: no empty path, no leading "/", no "." or ".." segment, no ".git"
// segment in any letter case, no empty segment, no whitespace at either end.
#AnchoredPath: string & !="" & !~"^/" & !~"//" & !~"(^|/)\\.\\.?(/|$)" & !~"(^|/)\\.(?i:git)(/|$)" & !~"^[\\s\\v\\pZ\\x{85}]|[\\s\\v\\pZ\\x{85}]$"

// Files another project holds, chosen by the caller. Written through the
// project's import: `content.#Files & {paths: ["articles/**"]}`. Paths (globs
// allowed) are relative to that project's directory. Reading them adds no
// dependency on any task of that project, and every path must match at least
// one file.
#ProjectFiles: {
	cuenvFiles:   "project"
	cuenvProject: string
	paths!: [#AnchoredPath, ...#AnchoredPath]
}

// Files anywhere in the CUE module that no project import reaches: lockfiles
// at the root, a directory several projects share. Paths (globs allowed) are
// relative to the module root, and every path must match at least one file.
#ModuleFiles: {
	cuenvFiles: "module"
	paths!: [#AnchoredPath, ...#AnchoredPath]
}

#Mapping: {
	from!: string
	to!:   string
}

// Reference to another task's outputs within the same project, by name.
// New configurations write `tasks.<name>.outputFiles` instead.
#TaskOutput: {
	task!: string
	map?: [...#Mapping]
}

// The declared outputs of one task, as an input of another: every path in
// that task's `outputs`. The schema writes it on every task as
// `outputFiles`; it is not written by hand. Reading it adds a dependency on
// the task.
#TaskOutputFilesRef: {
	cuenvOutputFilesRef: true
	cuenvProject:        string
	cuenvTask:           string
}

// =============================================================================
// Task Captures (Regex Extraction from Output)
// =============================================================================

// Regex capture from task output streams.
// The first capture group's match becomes the named value.
#TaskCapture: {
	pattern!: string
	source?:  "stdout" | "stderr" | *"stdout"
}

// Reference to a captured value, resolved at runtime by the executor.
// Usage in annotations:
//   annotations: "Preview URL": schema.#TaskCaptureRef & {
//       cuenvTask: "deploy.preview", cuenvCapture: "previewUrl"
//   }
#TaskCaptureRef: {
	cuenvCaptureRef: true
	cuenvTask!:      string
	cuenvCapture!:   string
}
