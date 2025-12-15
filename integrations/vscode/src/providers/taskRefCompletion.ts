import * as vscode from 'vscode';
import { CuenvClient } from '../client';
import { CuenvTask, WorkspaceTask } from '../types';

/**
 * Provides code completions for task references in CUE files.
 *
 * Supports completion in:
 * - `dependsOn: ["..."]` arrays (local tasks + cross-project refs)
 * - `beforeInstall: [{ ref: "..." }]` hooks (cross-project refs)
 * - `afterInstall: [{ ref: "..." }]` hooks (cross-project refs)
 * - `ref!: "..."` fields (cross-project refs)
 */
export class TaskRefCompletionProvider implements vscode.CompletionItemProvider {
    constructor(
        private client: CuenvClient,
        private outputChannel?: vscode.OutputChannel
    ) {}

    private log(message: string) {
        this.outputChannel?.appendLine(`[TaskRefCompletion] ${message}`);
    }

    async provideCompletionItems(
        document: vscode.TextDocument,
        position: vscode.Position,
        token: vscode.CancellationToken,
        context: vscode.CompletionContext
    ): Promise<vscode.CompletionItem[] | vscode.CompletionList | null> {
        // Only for CUE files
        if (document.languageId !== 'cue') {
            return null;
        }

        const completionContext = this.detectCompletionContext(document, position);
        if (!completionContext) {
            this.log('Not in a task reference context');
            return null;
        }

        this.log(`Detected context: ${completionContext.type}`);

        // Fetch both local and workspace tasks in parallel
        const [localTasks, workspaceTasks] = await Promise.all([
            this.client.getTasks(),
            this.client.getWorkspaceTasks(),
        ]);

        return this.buildCompletionItems(completionContext, workspaceTasks, localTasks);
    }

    private detectCompletionContext(
        document: vscode.TextDocument,
        position: vscode.Position
    ): TaskCompletionContext | null {
        const line = document.lineAt(position.line);
        const lineText = line.text;
        const textBefore = lineText.substring(0, position.character);

        // Check if we're inside a string
        if (!this.isInsideString(textBefore)) {
            return null;
        }

        // Get context lines for looking back
        const contextLines = this.getContextLines(document, position.line, 15);

        // Check for dependsOn context
        if (this.isInDependsOnContext(contextLines, lineText, position)) {
            return {
                type: 'dependsOn',
                lineText,
                position: position.character,
            };
        }

        // Check for beforeInstall/afterInstall hook context
        const hookType = this.isInHookContext(contextLines, lineText, position);
        if (hookType) {
            return {
                type: hookType,
                lineText,
                position: position.character,
            };
        }

        // Check for ref! field (TaskRef)
        if (this.isInTaskRefContext(textBefore)) {
            return {
                type: 'taskRef',
                lineText,
                position: position.character,
            };
        }

        return null;
    }

    private getContextLines(
        document: vscode.TextDocument,
        currentLine: number,
        lookBack: number
    ): string[] {
        const lines: string[] = [];
        const startLine = Math.max(0, currentLine - lookBack);
        for (let i = startLine; i <= currentLine; i++) {
            lines.push(document.lineAt(i).text);
        }
        return lines;
    }

    private isInDependsOnContext(
        contextLines: string[],
        currentLine: string,
        position: vscode.Position
    ): boolean {
        const contextText = contextLines.join('\n');

        // Match dependsOn: [ pattern
        const dependsOnPattern = /dependsOn:\s*\[[\s\S]*$/;
        if (!dependsOnPattern.test(contextText)) {
            return false;
        }

        // Make sure we haven't closed the array yet
        const afterDependsOn = contextText.split(/dependsOn:\s*\[/).pop() || '';
        const openBrackets = (afterDependsOn.match(/\[/g) || []).length;
        const closeBrackets = (afterDependsOn.match(/\]/g) || []).length;

        return openBrackets > closeBrackets;
    }

    private isInHookContext(
        contextLines: string[],
        currentLine: string,
        position: vscode.Position
    ): 'beforeInstall' | 'afterInstall' | null {
        const textBefore = currentLine.substring(0, position.character);

        // Check if we're inside a ref field value within a hook
        if (!/ref!?:\s*"[^"]*$/.test(textBefore)) {
            return null;
        }

        const contextText = contextLines.join('\n');

        if (/beforeInstall:\s*\[[\s\S]*$/.test(contextText)) {
            // Check bracket balance
            const afterHook = contextText.split(/beforeInstall:\s*\[/).pop() || '';
            const openBrackets = (afterHook.match(/\[/g) || []).length;
            const closeBrackets = (afterHook.match(/\]/g) || []).length;
            if (openBrackets > closeBrackets) {
                return 'beforeInstall';
            }
        }

        if (/afterInstall:\s*\[[\s\S]*$/.test(contextText)) {
            const afterHook = contextText.split(/afterInstall:\s*\[/).pop() || '';
            const openBrackets = (afterHook.match(/\[/g) || []).length;
            const closeBrackets = (afterHook.match(/\]/g) || []).length;
            if (openBrackets > closeBrackets) {
                return 'afterInstall';
            }
        }

        return null;
    }

    private isInTaskRefContext(textBefore: string): boolean {
        // Check for ref!: "... or ref: "... pattern
        return /ref!?:\s*"[^"]*$/.test(textBefore);
    }

    private isInsideString(textBefore: string): boolean {
        // Count unescaped quotes
        let inString = false;
        for (let i = 0; i < textBefore.length; i++) {
            if (textBefore[i] === '"' && (i === 0 || textBefore[i - 1] !== '\\')) {
                inString = !inString;
            }
        }
        return inString;
    }

    private buildCompletionItems(
        context: TaskCompletionContext,
        workspaceTasks: WorkspaceTask[],
        localTasks: CuenvTask[]
    ): vscode.CompletionItem[] {
        const items: vscode.CompletionItem[] = [];

        // For dependsOn, show both local tasks and cross-project refs
        if (context.type === 'dependsOn') {
            // Local tasks first (higher priority)
            for (const task of localTasks) {
                const item = new vscode.CompletionItem(
                    task.name,
                    task.is_group
                        ? vscode.CompletionItemKind.Folder
                        : vscode.CompletionItemKind.Function
                );
                item.detail = task.is_group ? '(task group)' : '(task)';
                item.documentation = task.description || `Local task: ${task.name}`;
                item.sortText = '0' + task.name; // Local tasks sort first
                items.push(item);
            }

            // Cross-project refs (lower priority)
            for (const task of workspaceTasks) {
                const item = new vscode.CompletionItem(
                    task.task_ref,
                    task.is_group
                        ? vscode.CompletionItemKind.Folder
                        : vscode.CompletionItemKind.Reference
                );
                item.detail = `${task.project} > ${task.task}`;
                item.documentation = new vscode.MarkdownString(
                    `**Project:** ${task.project}\n\n` +
                        `**Task:** ${task.task}\n\n` +
                        (task.description || '')
                );
                item.insertText = task.task_ref;
                item.filterText = `${task.task_ref} ${task.project} ${task.task}`;
                item.sortText = '1' + task.task_ref; // Cross-project refs sort after local
                items.push(item);
            }
        }

        // For hooks and ref fields, only show cross-project refs
        if (
            context.type === 'taskRef' ||
            context.type === 'beforeInstall' ||
            context.type === 'afterInstall'
        ) {
            for (const task of workspaceTasks) {
                const item = new vscode.CompletionItem(
                    task.task_ref,
                    task.is_group
                        ? vscode.CompletionItemKind.Folder
                        : vscode.CompletionItemKind.Reference
                );
                item.detail = `${task.project} > ${task.task}`;
                item.documentation = new vscode.MarkdownString(
                    `**Project:** ${task.project}\n\n` +
                        `**Task:** ${task.task}\n\n` +
                        (task.description || '')
                );
                item.insertText = task.task_ref;
                item.filterText = `${task.task_ref} ${task.project} ${task.task}`;
                items.push(item);
            }
        }

        return items;
    }
}

interface TaskCompletionContext {
    type: 'dependsOn' | 'taskRef' | 'beforeInstall' | 'afterInstall';
    lineText: string;
    position: number;
}
