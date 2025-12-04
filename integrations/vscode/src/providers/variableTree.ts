import * as vscode from 'vscode';
import { CuenvClient } from '../client';

export class VariableTreeDataProvider implements vscode.TreeDataProvider<VariableTreeItem> {
    private _onDidChangeTreeData = new vscode.EventEmitter<VariableTreeItem | undefined | void>();
    readonly onDidChangeTreeData = this._onDidChangeTreeData.event;
    private currentEnv: string = 'Base';

    constructor(private client: CuenvClient) {}

    refresh(env?: string) {
        if (env) {
            this.currentEnv = env;
        }
        this._onDidChangeTreeData.fire();
    }

    getTreeItem(element: VariableTreeItem): vscode.TreeItem {
        return element;
    }

    async getChildren(element?: VariableTreeItem): Promise<VariableTreeItem[]> {
        if (element) return [];

        try {
            // Use the tracked current environment
            const env = this.currentEnv;
            const vars = await this.client.getEnvironmentVariables(env);
            
            const keys = Object.keys(vars);
            if (keys.length === 0) {
                const item = new VariableTreeItem("No variables found", "", false);
                item.iconPath = new vscode.ThemeIcon('info');
                item.description = `Environment: ${env}`;
                return [item];
            }

            return keys.sort().map(key => {
                const value = vars[key];
                const isSecret = value === '[SECRET]' || (typeof value === 'object' && value !== null);
                const stringValue = typeof value === 'string' ? value : JSON.stringify(value);
                return new VariableTreeItem(key, stringValue, isSecret);
            });
        } catch (error) {
            const item = new VariableTreeItem("Error loading variables", String(error), false);
            item.iconPath = new vscode.ThemeIcon('error');
            return [item];
        }
    }
}

export class VariableTreeItem extends vscode.TreeItem {
    constructor(
        public readonly key: string,
        public readonly value: string,
        public readonly isSecret: boolean
    ) {
        super(key, vscode.TreeItemCollapsibleState.None);
        
        if (isSecret) {
            this.description = '********';
            this.tooltip = 'Secret value';
            this.iconPath = new vscode.ThemeIcon('lock');
        } else {
            this.description = value;
            this.tooltip = `${key}=${value}`;
            this.iconPath = new vscode.ThemeIcon('symbol-variable');
        }
        
        // Copy value on click logic could go here or via command
        this.command = {
            command: 'cuenv.copyVariable',
            title: 'Copy Value',
            arguments: [this]
        };
    }
}
