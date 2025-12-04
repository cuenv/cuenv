import * as vscode from 'vscode';
import * as cp from 'child_process';
import * as path from 'path';

interface CuenvTask {
    name: string;
    definition: any; // We can refine this later
    is_group: boolean;
    description?: string;
}

export function activate(context: vscode.ExtensionContext) {
    const outputChannel = vscode.window.createOutputChannel("Cuenv");
    const client = new CuenvClient(outputChannel);

    // Tree Data Providers
    const taskProvider = new TaskTreeDataProvider(client);
    const envProvider = new EnvTreeDataProvider(client);

    vscode.window.registerTreeDataProvider('cuenv.tasks', taskProvider);
    vscode.window.registerTreeDataProvider('cuenv.environments', envProvider);

    // Commands
    context.subscriptions.push(
        vscode.commands.registerCommand('cuenv.refresh', () => {
            taskProvider.refresh();
            envProvider.refresh();
        }),
        vscode.commands.registerCommand('cuenv.runTask', (item: TaskTreeItem) => {
            client.runTask(item.label as string);
        }),
        vscode.commands.registerCommand('cuenv.setEnvironment', (item: EnvTreeItem) => {
            client.setEnvironment(item.label as string);
            envProvider.refresh(); // Update UI to show selection
        })
    );

    // Initial refresh
    taskProvider.refresh();
    envProvider.refresh();
}

export function deactivate() {}

class CuenvClient {
    private currentEnv: string | undefined;

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
            const output = await this.execJson(['task', '--output-format', 'json'], root);
            return output as CuenvTask[];
        } catch (e) {
            this.outputChannel.appendLine(`Error fetching tasks: ${e}`);
            return [];
        }
    }

    async getEnvironments(): Promise<string[]> {
        const root = this.getWorkspaceRoot();
        if (!root) return [];

        try {
            const output = await this.execJson(['env', 'list', '--output-format', 'json'], root);
            return output as string[];
        } catch (e) {
            this.outputChannel.appendLine(`Error fetching environments: ${e}`);
            return [];
        }
    }

    runTask(taskName: string) {
        const root = this.getWorkspaceRoot();
        if (!root) return;

        const executable = this.getExecutable();
        const args = ['task', taskName];
        
        if (this.currentEnv) {
            args.push('--env', this.currentEnv);
        }

        // Create a new terminal for execution
        const terminal = vscode.window.createTerminal({
            name: `Cuenv: ${taskName}`,
            cwd: root,
            env: process.env // Inherit env to ensure path is correct
        });
        
        terminal.show();
        terminal.sendText(`${executable} ${args.join(' ')}`);
    }

    private execJson(args: string[], cwd: string): Promise<any> {
        return new Promise((resolve, reject) => {
            const executable = this.getExecutable();
            
            cp.execFile(executable, args, { cwd }, (error, stdout, stderr) => {
                if (error) {
                    reject(stderr || error.message);
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
}

// --- Task Tree View ---

class TaskTreeDataProvider implements vscode.TreeDataProvider<TaskTreeItem> {
    private _onDidChangeTreeData = new vscode.EventEmitter<TaskTreeItem | undefined | void>();
    readonly onDidChangeTreeData = this._onDidChangeTreeData.event;

    constructor(private client: CuenvClient) {}

    refresh() {
        this._onDidChangeTreeData.fire();
    }

    getTreeItem(element: TaskTreeItem): vscode.TreeItem {
        return element;
    }

    async getChildren(element?: TaskTreeItem): Promise<TaskTreeItem[]> {
        if (element) {
            return []; // Flat list for now, can implement hierarchy later
        }

        const tasks = await this.client.getTasks();
        // Filter out groups if they are just containers, or show them differently.
        // For now, let's just show everything.
        return tasks.map(t => new TaskTreeItem(
            t.name, 
            t.description || (t.is_group ? "Task Group" : "Task"),
            t.is_group ? vscode.TreeItemCollapsibleState.None : vscode.TreeItemCollapsibleState.None
        ));
    }
}

class TaskTreeItem extends vscode.TreeItem {
    constructor(
        public readonly label: string,
        public readonly description: string,
        public readonly collapsibleState: vscode.TreeItemCollapsibleState
    ) {
        super(label, collapsibleState);
        this.tooltip = `${this.label}: ${this.description}`;
        this.contextValue = 'task';
        this.iconPath = new vscode.ThemeIcon('checklist');
    }
}

// --- Environment Tree View ---

class EnvTreeDataProvider implements vscode.TreeDataProvider<EnvTreeItem> {
    private _onDidChangeTreeData = new vscode.EventEmitter<EnvTreeItem | undefined | void>();
    readonly onDidChangeTreeData = this._onDidChangeTreeData.event;

    constructor(private client: CuenvClient) {}

    refresh() {
        this._onDidChangeTreeData.fire();
    }

    getTreeItem(element: EnvTreeItem): vscode.TreeItem {
        return element;
    }

    async getChildren(element?: EnvTreeItem): Promise<EnvTreeItem[]> {
        if (element) return [];

        const envs = await this.client.getEnvironments();
        const current = this.client.getCurrentEnvironment();

        return envs.map(e => new EnvTreeItem(e, e === current));
    }
}

class EnvTreeItem extends vscode.TreeItem {
    constructor(
        public readonly label: string,
        public readonly isActive: boolean
    ) {
        super(label, vscode.TreeItemCollapsibleState.None);
        this.contextValue = 'environment';
        if (isActive) {
            this.iconPath = new vscode.ThemeIcon('check');
            this.description = '(Active)';
        } else {
            this.iconPath = new vscode.ThemeIcon('server');
        }
    }
}
