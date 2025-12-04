import * as vscode from 'vscode';
import { CuenvClient } from '../client';

export class CuenvCodeLensProvider implements vscode.CodeLensProvider {
    constructor(private client: CuenvClient) {}

    provideCodeLenses(document: vscode.TextDocument, token: vscode.CancellationToken): vscode.CodeLens[] {
        const lenses: vscode.CodeLens[] = [];
        const text = document.getText();
        
        // Regex to find task definitions:
        // Looking for: taskname: { ... } inside tasks: { ... }
        // This is a simplified regex approach. A real CUE parser would be better but heavier.
        // We look for lines that look like keys in the task structure.
        
        // Matches "key:" or "key: {" at start of line (ignoring whitespace)
        const keyRegex = /^\s*([\w.-]+):\s*{/gm;
        
        // We need to be careful not to match env vars or other structs.
        // For now, let's try to find specific task-like patterns or rely on `cuenv task` output mapping.
        
        // Better approach: Get the list of known tasks from the client, and search for them in the file.
        // This avoids false positives on random structs.
        
        // NOTE: Since provideCodeLenses is synchronous-ish (can return a Thenable), we can fetch tasks.
        // But calling CLI on every keystroke/scroll is bad. We should cache tasks.
        // For this iteration, let's stick to a simple heuristic:
        // If the file is `env.cue`, scan for task definitions.
        
        if (!document.fileName.endsWith('env.cue')) {
            return [];
        }

        // Find the "tasks:" block
        const lines = text.split('\n');
        let insideTasks = false;
        let indentLevel = 0;

        for (let i = 0; i < lines.length; i++) {
            const line = lines[i];
            const trimmed = line.trim();
            
            if (trimmed.startsWith('tasks:') && trimmed.endsWith('{')) {
                insideTasks = true;
                indentLevel = line.indexOf('tasks:');
                continue;
            }

            if (insideTasks) {
                if (trimmed === '}') {
                    // Check indentation to see if we closed the tasks block
                    // Simple heuristic: if '}' is at same indent as 'tasks:', we are done.
                    if (line.indexOf('}') === indentLevel) {
                        insideTasks = false;
                        break;
                    }
                }

                // Match task definitions: "build: {" or "test: {"
                const match = /^\s*([\w.-]+):\s*{/.exec(line);
                if (match) {
                    const taskName = match[1];
                    const range = new vscode.Range(i, 0, i, line.length);
                    
                    const cmd: vscode.Command = {
                        title: `$(play) Run ${taskName}`,
                        command: 'cuenv.runTask',
                        arguments: [taskName] // This assumes the local name matches the task name, ignoring nesting for now
                    };
                    
                    lenses.push(new vscode.CodeLens(range, cmd));
                }
            }
        }

        return lenses;
    }
}
