package schema

// #Policy defines access control for environment variables
#Policy: {
	// Allowlist of task names that can access this variable
	allowTasks?: [...string]

	// Allowlist of exec commands that can access this variable
	allowExec?: [...string]
}