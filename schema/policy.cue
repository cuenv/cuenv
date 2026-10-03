package schema

// An infrastructure command that can be named in `allowInfrastructure`.
// The names match the `cuenv infrastructure` subcommands: `state-list`,
// `state-remove`, `state-recover` and `state-adopt` are the `state`
// subcommands.
#InfrastructureAction: "plan" | "apply" | "destroy" | "state-list" | "state-remove" | "state-recover" | "state-adopt" | "unlock"

// #Policy defines access control for environment variables
#Policy: close({
	// Allowlist of task names that can access this variable
	allowTasks?: [...string]

	// Allowlist of exec commands that can access this variable
	allowExec?: [...string]

	// Allowlist of infrastructure actions that can access this variable.
	// A variable that has policies is withheld from an infrastructure command
	// unless some policy lists that command's action; `allowTasks` and
	// `allowExec` do not grant it.
	allowInfrastructure?: [...#InfrastructureAction]
})
