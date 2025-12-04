import * as vscode from 'vscode';
import * as path from 'path';
import * as fs from 'fs';
import { CuenvClient } from './client';
import { TaskTreeDataProvider, TaskTreeItem } from './providers/taskTree';
import { EnvTreeDataProvider, EnvTreeItem } from './providers/envTree';
import { VariableTreeDataProvider, VariableTreeItem } from './providers/variableTree';
import { CuenvCodeLensProvider } from './providers/codelens';
import { GraphWebview } from './webview/graph';

export function activate(context: vscode.ExtensionContext) {
    const outputChannel = vscode.window.createOutputChannel("Cuenv");
    const client = new CuenvClient(outputChannel);

    // Providers
    const taskProvider = new TaskTreeDataProvider(client);
    const envProvider = new EnvTreeDataProvider(client);
    const variableProvider = new VariableTreeDataProvider(client);

    // Register Tree Data Providers
    vscode.window.registerTreeDataProvider('cuenv.tasks', taskProvider);
    vscode.window.registerTreeDataProvider('cuenv.environments', envProvider);
    vscode.window.registerTreeDataProvider('cuenv.variables', variableProvider);

    // Register CodeLens
    context.subscriptions.push(
        vscode.languages.registerCodeLensProvider(
            { language: 'cue', scheme: 'file' }, // Only CUE files
            new CuenvCodeLensProvider(client)
        )
    );

    // Commands
    context.subscriptions.push(
        vscode.commands.registerCommand('cuenv.refresh', () => {
            taskProvider.refresh();
            envProvider.refresh();
            variableProvider.refresh(); // Refresh variables for current env
        }),
        vscode.commands.registerCommand('cuenv.runTask', (arg: string | TaskTreeItem) => {
            // Can be called from TreeItem or CodeLens (string)
            const taskName = typeof arg === 'string' ? arg : arg.fullTaskName;
            client.runTask(taskName);
        }),
        vscode.commands.registerCommand('cuenv.setEnvironment', (item: EnvTreeItem) => {
            const envName = item.label as string;
            client.setEnvironment(envName);
            envProvider.refresh();
            variableProvider.refresh(envName); // Switch variables view to this env
        }),
        vscode.commands.registerCommand('cuenv.showGraph', () => {
            GraphWebview.show(context.extensionUri, client);
        }),
        vscode.commands.registerCommand('cuenv.copyVariable', (item: VariableTreeItem) => {
            if (item.isSecret) {
                vscode.window.showWarningMessage('Cannot copy secret value.');
                return;
            }
            vscode.env.clipboard.writeText(item.value);
            vscode.window.showInformationMessage(`Copied ${item.key} to clipboard.`);
        })
    );

    // Refresh Action
    const refreshAction = () => {
        taskProvider.refresh();
        envProvider.refresh();
        variableProvider.refresh();
    };

    // 1. Watch workspace CUE files
    const workspaceWatcher = vscode.workspace.createFileSystemWatcher('**/*.cue');
    context.subscriptions.push(workspaceWatcher);
    workspaceWatcher.onDidChange(refreshAction);
    workspaceWatcher.onDidCreate(refreshAction);
    workspaceWatcher.onDidDelete(refreshAction);

    // 2. Watch parent env.cue files
    const workspaceRoot = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
    if (workspaceRoot) {
        let currentDir = path.dirname(workspaceRoot);
        // Walk up until we hit the root (parent is same as current)
        while (currentDir !== path.dirname(currentDir)) {
            const envFile = path.join(currentDir, 'env.cue');
            if (fs.existsSync(envFile)) {
                const watcher = vscode.workspace.createFileSystemWatcher(new vscode.RelativePattern(currentDir, 'env.cue'));
                context.subscriptions.push(watcher);
                watcher.onDidChange(refreshAction);
                watcher.onDidCreate(refreshAction);
                watcher.onDidDelete(refreshAction);
            }
            currentDir = path.dirname(currentDir);
        }
    }

    // Initial refresh
    taskProvider.refresh();
    envProvider.refresh();
    variableProvider.refresh('Base'); // Default to Base
}

export function deactivate() {}