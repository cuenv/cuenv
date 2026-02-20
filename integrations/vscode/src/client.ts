import * as vscode from 'vscode';
import * as cp from 'child_process';
import { CuenvTask, WorkspaceTask } from './types';
import {
    BASE_ENVIRONMENT,
    buildEnvironmentListArgs,
    buildEnvironmentPrintArgs,
    buildTaskListArgs,
    buildTaskRunArgs,
    buildWorkspaceTaskListArgs
} from './cuenvArgs';

export class CuenvClient {
    private currentEnv: string | undefined = BASE_ENVIRONMENT;

    // Cache for workspace tasks (used by completion provider)
    private workspaceTasksCache: WorkspaceTask[] | null = null;
    private workspaceTasksCacheTime: number = 0;
    private readonly CACHE_TTL_MS = 30000; // 30 seconds

    constructor(private outputChannel: vscode.OutputChannel) {}

    private getExecutable(): string {
        return vscode.workspace.getConfiguration('cuenv').get('executablePath') || 'cuenv';
    }

    private getWorkspaceRoot(): string | undefined {
        return vscode.workspace.workspaceFolders?.[0].uri.fsPath;
    }

    setEnvironment(env: string) {
        this.currentEnv = env;
        vscode.window.showInformationMessage(`Cuenv environment set to: ${env}`);
    }

    async getTasks(): Promise<CuenvTask[]> {
        const root = this.getWorkspaceRoot();
        if (!root) return [];

        try {
            const output = await this.execJson(buildTaskListArgs(this.currentEnv), root);
            return output as CuenvTask[];
        } catch (e) {
            this.outputChannel.appendLine(`Error fetching tasks: ${e}`);
            return [];
        }
    }

    async getEnvironments(): Promise<string[]> {
        const root = this.getWorkspaceRoot();
        if (!root) return [BASE_ENVIRONMENT];

        try {
            const output = await this.execJson(buildEnvironmentListArgs(), root);
            const envs = output as string[];
            // Always ensure Base is present
            if (!envs.includes(BASE_ENVIRONMENT)) {
                envs.unshift(BASE_ENVIRONMENT);
            }
            return envs;
        } catch (e) {
            this.outputChannel.appendLine(`Error fetching environments: ${e}`);
            return [BASE_ENVIRONMENT];
        }
    }

    runTask(taskName: string) {
        const root = this.getWorkspaceRoot();
        if (!root) return;

        const executable = this.getExecutable();
        const args = buildTaskRunArgs(taskName, this.currentEnv);

        const terminal = vscode.window.createTerminal({
            name: `Cuenv: ${taskName}`,
            cwd: root,
            env: process.env,
            shellPath: executable,
            shellArgs: args
        });
        
        terminal.show();
    }

    async getEnvironmentVariables(envName?: string): Promise<Record<string, string>> {
        const root = this.getWorkspaceRoot();
        if (!root) return {};

        const targetEnv = envName || this.currentEnv;
        const args = buildEnvironmentPrintArgs(targetEnv);

        try {
            return await this.execJson(args, root);
        } catch (e) {
            this.outputChannel.appendLine(`Error fetching environment variables: ${e}`);
            return {};
        }
    }

    private execJson(args: string[], cwd: string): Promise<any> {
        return new Promise((resolve, reject) => {
            const executable = this.getExecutable();
            
            cp.execFile(executable, args, { cwd }, (error, stdout, stderr) => {
                if (error) {
                    const stderrOutput = stderr.trim();
                    const stdoutOutput = stdout.trim();
                    const message = stderrOutput
                        || `${error.message}${stdoutOutput ? `\nStdout: ${stdoutOutput}` : ''}`;
                    reject(message);
                    return;
                }
                try {
                    resolve(JSON.parse(stdout));
                } catch (e) {
                    reject(`Failed to parse JSON: ${e}\nOutput: ${stdout}`);
                }
            });
        });
    }

    getCurrentEnvironment(): string | undefined {
        return this.currentEnv;
    }

    /**
     * Get all tasks from all projects in the workspace.
     * Results are cached for 30 seconds.
     */
    async getWorkspaceTasks(forceRefresh = false): Promise<WorkspaceTask[]> {
        const now = Date.now();
        if (
            !forceRefresh &&
            this.workspaceTasksCache &&
            now - this.workspaceTasksCacheTime < this.CACHE_TTL_MS
        ) {
            return this.workspaceTasksCache;
        }

        const root = this.getWorkspaceRoot();
        if (!root) return [];

        try {
            const output = await this.execJson(buildWorkspaceTaskListArgs(this.currentEnv), root);
            this.workspaceTasksCache = output as WorkspaceTask[];
            this.workspaceTasksCacheTime = now;
            return this.workspaceTasksCache;
        } catch (e) {
            this.outputChannel.appendLine(`Error fetching workspace tasks: ${e}`);
            // Return stale cache if available, otherwise empty array
            return this.workspaceTasksCache || [];
        }
    }

    /**
     * Invalidate the workspace tasks cache.
     * Call this when CUE files change.
     */
    invalidateTaskCache(): void {
        this.workspaceTasksCache = null;
        this.workspaceTasksCacheTime = 0;
    }
}
