import * as vscode from 'vscode';
import { CuenvClient } from '../client';
import { CuenvTask } from '../types';

interface TaskNode {
    segment: string;
    fullPath: string;
    task?: CuenvTask;
    children: Map<string, TaskNode>;
}

export class TaskTreeDataProvider implements vscode.TreeDataProvider<TaskTreeItem> {
    private _onDidChangeTreeData = new vscode.EventEmitter<TaskTreeItem | undefined | void>();
    readonly onDidChangeTreeData = this._onDidChangeTreeData.event;
    private taskTree: Map<string, TaskNode> = new Map();

    constructor(private client: CuenvClient) {}

    refresh() {
        this.taskTree.clear();
        this._onDidChangeTreeData.fire();
    }

    getTreeItem(element: TaskTreeItem): vscode.TreeItem {
        return element;
    }

    async getChildren(element?: TaskTreeItem): Promise<TaskTreeItem[]> {
        if (!element) {
            const tasks = await this.client.getTasks();
            this.buildTaskTree(tasks);
            return this.getTreeItems(this.taskTree);
        }

        const node = element.node;
        return this.getTreeItems(node.children);
    }

    private buildTaskTree(tasks: CuenvTask[]) {
        this.taskTree = new Map();

        for (const task of tasks) {
            const parts = task.name.split('.');
            let currentLevel = this.taskTree;

            for (let i = 0; i < parts.length; i++) {
                const part = parts[i];
                const isLast = i === parts.length - 1;
                const fullPath = parts.slice(0, i + 1).join('.');

                if (!currentLevel.has(part)) {
                    currentLevel.set(part, {
                        segment: part,
                        fullPath: fullPath,
                        children: new Map()
                    });
                }

                const node = currentLevel.get(part)!;

                if (isLast) {
                    node.task = task;
                }

                currentLevel = node.children;
            }
        }
    }

    private getTreeItems(nodes: Map<string, TaskNode>): TaskTreeItem[] {
        const items: TaskTreeItem[] = [];
        for (const node of nodes.values()) {
            items.push(new TaskTreeItem(node));
        }
        return items.sort((a, b) => {
            const aHasChildren = a.node.children.size > 0;
            const bHasChildren = b.node.children.size > 0;
            if (aHasChildren && !bHasChildren) return -1;
            if (!aHasChildren && bHasChildren) return 1;
            
            const labelA = typeof a.label === 'string' ? a.label : a.label?.label || '';
            const labelB = typeof b.label === 'string' ? b.label : b.label?.label || '';
            return labelA.localeCompare(labelB);
        });
    }
}

export class TaskTreeItem extends vscode.TreeItem {
    constructor(public readonly node: TaskNode) {
        const hasChildren = node.children.size > 0;
        super(
            node.segment,
            hasChildren ? vscode.TreeItemCollapsibleState.Collapsed : vscode.TreeItemCollapsibleState.None
        );

        this.contextValue = node.task ? 'task' : 'group';
        
        if (node.task) {
            const desc = node.task.definition?.description || node.task.description;
            this.description = desc;
            this.tooltip = `${node.fullPath}${desc ? ': ' + desc : ''}`;
            
            if (node.task.is_group) {
                this.iconPath = new vscode.ThemeIcon('repo');
            } else {
                this.iconPath = new vscode.ThemeIcon('play-circle');
            }
        } else {
            this.tooltip = node.fullPath;
            this.iconPath = vscode.ThemeIcon.Folder;
        }
    }

    get fullTaskName(): string {
        return this.node.fullPath;
    }
}
