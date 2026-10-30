package schema

// An infrastructure command that can be named in `allowInfrastructure`.
// The names match the `cuenv infrastructure` subcommands: the `state-` names
// are the `state` subcommands. `state-adopt` is retired (the command does
// nothing) and stays accepted so existing policies keep evaluating.
#InfrastructureAction: "plan" | "apply" | "import" | "destroy" | "state-list" | "state-remove" | "state-recover" | "state-adopt" | "state-pull" | "state-restore" | "state-migrate" | "state-rename" | "unlock"

// #Policy defines access control for environment variables
#Policy: {
	// Allowlist of task names that can access this variable
	allowTasks?: [...string]

	// Allowlist of exec commands that can access this variable
	allowExec?: [...string]

	// Allowlist of infrastructure actions that can access this variable.
	// A variable that has policies is withheld from an infrastructure command
	// unless some policy lists that command's action; `allowTasks` and
	// `allowExec` do not grant it.
	allowInfrastructure?: [...#InfrastructureAction]
}
