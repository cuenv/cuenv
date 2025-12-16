export interface CuenvTask {
    name: string;
    definition: TaskDefinition;
    is_group: boolean;
    description?: string;
}

/**
 * Task reference for workspace-wide task listing (used by IDE completions).
 * Returned by `cuenv task --all --output-format json`.
 */
export interface WorkspaceTask {
    /** Project name from env.cue `name` field */
    project: string;
    /** Task name within the project (canonical dotted path) */
    task: string;
    /** Full task reference string in format "#project:task" */
    task_ref: string;
    /** Task description if available */
    description: string | null;
    /** Whether this is a task group */
    is_group: boolean;
}

export interface TaskDefinition {
    shell?: {
        command?: string;
        flag?: string;
    };
    command?: string;
    args?: string[];
    env?: Record<string, any>;
    hermetic?: boolean;
    dependsOn?: string[];
    inputs?: string[];
    outputs?: string[];
    description?: string;
    // Group definitions can have nested tasks, but the CLI output currently flattens groups
    // or returns the raw structure for 'definition'.
    // If is_group is true, definition might be an object of TaskDefinitions or array.
    // However, cuenv task --output-format json returns a flat list of all addressable tasks.
    // We primarily care about single tasks for graph visualization.
}
