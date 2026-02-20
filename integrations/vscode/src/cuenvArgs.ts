export const BASE_ENVIRONMENT = 'Base';

/**
 * Build args for `cuenv task` list in JSON mode.
 */
export function buildTaskListArgs(environment?: string): string[] {
    return withGlobalEnvironment(['task', '--output', 'json'], environment);
}

/**
 * Build args for `cuenv task --all` list in JSON mode.
 */
export function buildWorkspaceTaskListArgs(environment?: string): string[] {
    return withGlobalEnvironment(['task', '--all', '--output', 'json'], environment);
}

/**
 * Build args for `cuenv env list` in JSON mode.
 */
export function buildEnvironmentListArgs(): string[] {
    return ['env', 'list', '--output', 'json'];
}

/**
 * Build args for `cuenv env print` in JSON mode.
 * Uses global `--env` before the command so clap always treats it as global.
 */
export function buildEnvironmentPrintArgs(environment?: string): string[] {
    return withGlobalEnvironment(['env', 'print', '--output', 'json'], environment);
}

/**
 * Build args for `cuenv task <name>`.
 * Uses global `--env` before the command so it is not consumed as a task arg.
 */
export function buildTaskRunArgs(taskName: string, environment?: string): string[] {
    return withGlobalEnvironment(['task', taskName], environment);
}

function withGlobalEnvironment(args: string[], environment?: string): string[] {
    if (!environment || environment === BASE_ENVIRONMENT) {
        return args;
    }

    return ['--env', environment, ...args];
}
