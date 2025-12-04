import * as vscode from 'vscode';
import { CuenvClient } from '../client';

export class EnvTreeDataProvider implements vscode.TreeDataProvider<EnvTreeItem> {
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

export class EnvTreeItem extends vscode.TreeItem {
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
